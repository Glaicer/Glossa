# Changelog

All notable changes to Glossa will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.4.0] - 2026-06-20

### Added

- **Fedora 44 support.** Glossa now installs and runs on Fedora 44 in addition to Ubuntu. The tray no longer hard-codes Ubuntu, the install and uninstall scripts detect the system package manager (`apt-get`, `dnf`, `yum`) and use the matching package names per distro
- `curl` as an alternative to `wget` in the updater, so updates work on systems without `wget` installed.

## [1.3.0] - 2026-06-16

### Added

- Glossa now preserves clipboard content before attempting to paste the transcribed text and restores the original clipboard contents after the paste completes.

## [1.2.2] - 2026-06-13

### Fixed

- Prevent the LLM enhancer from following instructions embedded in the transcript.

## [1.2.1] - 2026-06-04

### Fixed

- Transcribed text sometimes failed to paste automatically because the paste key chord was triggered too quickly or simultaneously. Replaced the `dotool` key chord with successive keystrokes and added a small delay to better emulate human typing.

## [1.2.0] - 2026-05-06

### Added

- Optional AI enhancer for transcribed texts. Each transcription can optionally be run through an OpenAI-compatible LLM before being pasted, to fix punctuation and obvious transcription mistakes.

## [1.1.0] - 2026-04-25

This release makes Glossa feel smoother in day-to-day use: continuous dictation is easier, recording starts faster, audio behavior is more configurable, and the tray settings dialog is less cramped.

### Highlights

- Optional trailing-space paste for smoother continuous dictation.
- Esc-based recording cancel.
- Faster, more reliable recording startup with less risk of clipping the start of a phrase.
- Microphone keepalive controls to reduce cold-start latency.
- Audio cue controls for people who want silent recording.
- Optional captured-audio retention for debugging.
- A cleaner, more comfortable tray `Settings` dialog.

### Added

- New `paste.append_space` setting. When enabled, Glossa adds a trailing space after the pasted transcription text, making continuous dictation less awkward.
- New `Append space` toggle in the tray `Settings` dialog.
- New `audio.persist_audio` debugging setting. When enabled, captured audio files remain in the configured work directory instead of being deleted after each recording cycle or during startup cleanup.
- New `audio.enabled` setting. When disabled, Glossa skips cue playback and does not keep an audio output stream open for start and stop sounds.
- Esc-based recording cancel: pressing Esc while recording stops capture, plays the normal stop cue, deletes the captured audio file, skips transcription and paste, and returns Glossa to idle.
- New `audio.latency_mode` and `audio.keepalive_after_stop_seconds` settings for reducing microphone cold-start latency.
- New tray `Mic stream` toggle.
- New `glossa ctl stream` command for enabling or disabling the idle microphone keepalive stream on demand.

### Changed

- The paste pipeline now writes `text + " "` to the clipboard only when `append_space = true`. The existing paste shortcut flow is unchanged.
- The recording start sequence now begins capture before the start cue plays, removing the extra delay that could clip the beginning of a phrase.
- Cue playback now keeps the output stream alive between sounds and retries once after an output-stream failure, eliminating the repeated first-cue delay after idle without making cue errors fatal to recording.
- Audio capture startup now prefers lower-latency input buffer sizes, waits for the first input samples before proceeding, and can keep an idle input stream alive for a short time after recording stops.
- The tray `Settings` dialog has more breathing room, better-aligned labels, padded section bodies, and less cramped action buttons.

## [1.0.1] - 2026-04-23

This patch release fixes a paste regression on some GNOME/Wayland systems where Glossa could stop pasting even though `dotool` was installed and running.

### Fixed

- Switched the default paste command from `dotool` to `dotoolc`, so Glossa now sends paste actions through the already-running `dotoold` service.
- Updated installer-generated config to use `dotoolc` by default.
- Updated the example config and runtime defaults to match the working paste path.
- Fixed `glossa doctor` so it checks the configured paste command instead of always checking hardcoded `dotool`.

### Notes

- `dotool` can be sensitive to device-registration timing when started as a one-shot process. Using `dotoolc` with the persistent `dotoold` service is more reliable and fixes cases where paste suddenly stopped working on the host machine while still working elsewhere.

## [1.0.0] - 2026-04-20

After a long stretch of testing and cleanup, Glossa v1.0.0 is out.

Glossa started with a specific problem: speech-to-text on Ubuntu + GNOME + Wayland was more annoying than it should have been. Existing tools like `wtype` and `ydotool` worked around parts of it, but the overall experience still felt fragile. Glossa was built to make that flow simple: press a hotkey, speak, and paste the transcription straight into whatever field is focused.

### Added

- Wayland-friendly clipboard and paste flow using `dotool` plus standard shortcuts (`Ctrl+V`, `Shift+Insert`), avoiding many of the virtual keyboard and layout problems that show up with non-English input.
- Global shortcuts through the XDG Desktop Portal, with support for both push-to-talk and toggle recording modes.
- Support for Groq, OpenAI, and other OpenAI-compatible or self-hosted STT APIs. Groq remains the recommended default for the easiest fast setup.
- System tray menu with status icons and quick access to settings and shortcut rebinding.
- Autostart at login via user-session systemd services for `glossa` and `dotool`.
- Built-in updater that can install the latest stable release from the tray menu or from the command line.
- CLI tools for setup and troubleshooting: `glossa doctor`, `glossa status`, and `glossa ctl`.

## [0.3.0] - 2026-04-19 (Pre-release)

### Added

- Glossa updater. An existing installation can be updated in any of these ways:
  - `bash <(wget -qO- https://raw.githubusercontent.com/Glaicer/glossa/main/update.sh)`
  - `glossa update`
  - Tray menu: `Update`

  The updater downloads the latest stable release, verifies its checksum, replaces the Glossa binary and bundled assets, and restarts `glossa.service`.

## [0.2.0] - 2026-04-18 (Pre-release)

First public release of Glossa: a headless speech-to-text daemon for Ubuntu + GNOME + Wayland.

Glossa lets you hold or toggle a global shortcut, record microphone input, transcribe it with Groq or OpenAI, copy the result to the Wayland clipboard, and paste it directly into the active window.

### Added

- Global shortcut support through the XDG Desktop Portal.
- Push-to-talk and toggle recording modes.
- Groq, OpenAI, and OpenAI-compatible transcription providers.
- Automatic clipboard + paste flow for Wayland.
- User-session systemd services for `glossa` and `dotool`.
- Tray integration with status icons and shortcut rebinding.
- `glossa doctor`, `glossa status`, and `glossa ctl` commands.

[1.4.0]: https://github.com/Glaicer/Glossa/releases/tag/v1.4.0
[1.3.0]: https://github.com/Glaicer/Glossa/releases/tag/v1.3.0
[1.2.2]: https://github.com/Glaicer/Glossa/releases/tag/v1.2.2
[1.2.1]: https://github.com/Glaicer/Glossa/releases/tag/v1.2.1
[1.2.0]: https://github.com/Glaicer/Glossa/releases/tag/v1.2.0
[1.1.0]: https://github.com/Glaicer/Glossa/releases/tag/v1.1.0
[1.0.1]: https://github.com/Glaicer/Glossa/releases/tag/v1.0.1
[1.0.0]: https://github.com/Glaicer/Glossa/releases/tag/v1.0.0
[0.3.0]: https://github.com/Glaicer/Glossa/releases/tag/v0.3.0
[0.2.0]: https://github.com/Glaicer/Glossa/releases/tag/v0.2.0
