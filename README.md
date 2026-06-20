<p align="center">
  <img src="resources/glossa-logo.png" alt="glossa" height="80">
</p>

<p align="left" style="border-left: 3px solid; padding-left: 12px; margin-left: 12px; font-size: 16px; font-weight: 600; margin-block: 20px">
  <em>Press a hotkey, speak, and drop the transcription straight into whatever field is focused</em>
</p>

Glossa is a free, open source speech-to-text daemon for Ubuntu on GNOME Wayland. It records audio from your microphone, sends it to a speech-to-text provider, copies the result to the clipboard, and pastes it into the active window.

It works with Groq, OpenAI, and other OpenAI-compatible speech-to-text providers, including self-hosted ones.

I personally recommend using [Groq](https://groq.com/) as it offers the following benefits:

- Generous free limits (2,000 requests per day) and low prices (starting at $0.04 per hour) when you need more
- Amazingly fast LPU, capable of processing minutes of audio in seconds
- Whisper V3 Large and Whisper V3 Large Turbo, which support multilingual input out of the box

## Features

- Starts automatically with your system and works as a background daemon. Pastes transcribed text to the active input inside any window.
- Supports both toggle and push-to-talk modes.
- Works with Groq, OpenAI, and other OpenAI-compatible STT providers (including self-hosted setups).
- Can optionally run each transcription through an OpenAI-compatible LLM enhancer before pasting, to fix punctuation and obvious transcription mistakes.
- Easily configurable via `config.toml`.
- Manage the installed user service with `glossa service start`, `stop`, and `restart`.
- Includes microphone keepalive controls for lower recording startup latency.
- Pastes using standard clipboard shortcuts such as `Ctrl+V`, `Ctrl+Shift+V`, or `Shift+Insert`, which helps with non-English input without switching keyboard layouts.
- Restores initial clipboard content after pasting transcribed text.

## Installation

Run the interactive installer:

```
bash <(wget -qO- https://raw.githubusercontent.com/Glaicer/glossa/main/install.sh)
```

The script will automatically install runtime dependencies if missing and configure Glossa via terminal interface.

You may need to log out and back in before paste works if `dotool` was installed during the script run.

## Why I built this

I started Glossa because speech-to-text on GNOME Wayland still feels more awkward than it should.

Here are the issues I kept running into with other tools:

- On Wayland, apps usually cannot register true global hotkeys directly, so you often end up depending on desktop portal support.
- A popular library `wtype` fails in Wayland throwing: "Compositor does not support the virtual keyboard protocol".
- Another popular library `ydotool` throws `ydotoold backend unavailable (may have latency+delay issues)`.
- You can fix `ydotool` installation, but it still has issues: it doesn't support many non-English keyboard layouts and it has broken keybindings which prevents pasting from the clipboard via shortcuts.
- `dotool` has worked for me, however it is shipped as the source code and must be compiled manually.
- There are some very decent apps with local (gguf) STT models support, however these models are slower and less accurate on laptops than cloud APIs.

Glossa is my attempt to make this whole workflow simpler and more reliable on Ubuntu GNOME Wayland.

## Roadmap

Planned features:

- direct input support
- FLAC recording

## CLI Commands

Glossa provides these CLI commands:

- `glossa daemon` runs the daemon in the foreground. It requires `--config <path>`.
- `glossa service start` starts the installed `glossa` systemd user service.
- `glossa service stop` stops the installed `glossa` systemd user service.
- `glossa service restart` restarts the installed `glossa` systemd user service.
- `glossa ctl toggle` sends a toggle-recording command to the running daemon over IPC.
- `glossa ctl stream` toggles the warm microphone stream, like the tray `Mic stream` item.
- `glossa ctl shutdown` asks the running daemon to shut down over IPC.
- `glossa doctor` runs environment and configuration diagnostics.
- `glossa status` prints the current daemon status reported over IPC.
- `glossa update` downloads and installs the latest release.

## Configuration

Most settings are available from the tray `Settings` menu, so you usually do not need to edit the config file by hand.

If you want to configure Glossa manually, edit `~/.config/glossa/config.toml`. Use [config.toml](contrib/examples/config.toml) in this repository as the reference example.

The tray has an `AI enhancer` checkbox. When it is off, Glossa pastes the speech-to-text result directly. When it is on, Glossa sends the transcribed text to the OpenAI-compatible chat endpoint first, then pastes the corrected response. Configure `enabled`, `base_url`, `model`, and `api_key` in the tray Settings → `LLM enhancer` window or in the `[LLM]` section of `config.toml`.

## Updating

Update an existing installation in any of these ways:

- `bash <(wget -qO- https://raw.githubusercontent.com/Glaicer/glossa/main/update.sh)`
- `glossa update`
- tray menu: `Update`

The updater downloads the latest stable release, verifies its checksum, replaces the Glossa binary and bundled assets, and restarts `glossa.service`.

## Uninstalling

Run the interactive uninstaller:

```
bash <(wget -qO- https://raw.githubusercontent.com/Glaicer/glossa/main/uninstall.sh)
```

The script will:

- stop and remove the `glossa` and `dotool` user services
- remove the Glossa binary and bundled assets
- remove the generated Glossa config, or restore the most recent config backup if one exists
- keep a non-installer-managed `~/.config/glossa/config.toml` in place
- optionally remove `wl-clipboard` and `dotool`

## Requirements

Glossa depends on:

- `wl-copy` and `wl-paste` from `wl-clipboard`
- `dotool`

The installer checks for both and installs them automatically.

## Manual Installation

If you do not want to use `install.sh`, you can install the release bundle manually on Ubuntu GNOME Wayland:

1. Download `glossa-linux-x86_64.tar.gz` and `sha256sums.txt` from the latest GitHub release.
2. Verify the archive:

```bash
sha256sum -c --ignore-missing sha256sums.txt
```

3. Extract it:

```bash
tar -xzf glossa-linux-x86_64.tar.gz
```

4. Install the runtime dependencies that `install.sh` normally checks for:

```bash
sudo apt-get update
sudo apt-get install -y wl-clipboard libnotify-bin libxdo3
```

5. Install the bundled `dotool` payload and udev rule:

```bash
sudo install -Dm755 dotool/dotool /usr/local/bin/dotool
sudo install -Dm755 dotool/dotoolc /usr/local/bin/dotoolc
sudo install -Dm755 dotool/dotoold /usr/local/bin/dotoold
sudo install -Dm644 dotool/80-dotool.rules /etc/udev/rules.d/80-dotool.rules
sudo groupadd -f input
sudo usermod -a -G input "$USER"
printf 'uinput\n' | sudo tee /etc/modules-load.d/dotool.conf >/dev/null
sudo modprobe uinput
sudo udevadm control --reload
sudo udevadm trigger
```

6. Install Glossa itself under your user account:

```bash
mkdir -p ~/.local/share/glossa/assets/tray ~/.local/share/glossa/assets/sounds
install -Dm755 glossa ~/.local/bin/glossa
install -Dm755 update.sh ~/.local/bin/update.sh
install -Dm644 VERSION ~/.local/share/glossa/VERSION
install -m644 assets/tray/* ~/.local/share/glossa/assets/tray/
install -m644 assets/sounds/* ~/.local/share/glossa/assets/sounds/
```

7. Create `~/.config/glossa/config.toml`. A good reference is `contrib/examples/config.toml` from this repository. Set `type_command = "/usr/local/bin/dotoolc"` so Glossa sends paste actions through the running `dotoold` service. If you use `api_key = "env:..."`, also provide the variable through `~/.config/glossa/glossa.env` or your user session environment.
8. Create the user services and enable them:

```bash
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/dotool.service <<'EOF'
[Unit]
Description=dotool daemon
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=simple
ExecStart=/usr/local/bin/dotoold
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
EOF

cat > ~/.config/systemd/user/glossa.service <<'EOF'
[Unit]
Description=glossa user-session daemon
PartOf=graphical-session.target
After=graphical-session.target dotool.service
Wants=dotool.service

[Service]
Type=simple
EnvironmentFile=-%h/.config/glossa/glossa.env
ExecStart=%h/.local/bin/glossa --config %h/.config/glossa/config.toml daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
EOF

systemctl --user daemon-reload
systemctl --user enable --now dotool.service
systemctl --user enable --now glossa.service
```

If `dotool` was installed for the first time, log out and back in before expecting paste to work.

## Development

For local development, use the example config and standard Cargo commands:

```bash
cargo build
cargo test --workspace
cargo clippy --workspace
cargo run --package glossa-bin -- --config contrib/examples/config.toml daemon
```

The example config already points at the checked-out `contrib/assets/...` files, so it is meant for running from the repository rather than from an installed release.
