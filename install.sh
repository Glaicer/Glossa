#!/usr/bin/env bash
set -euo pipefail

readonly GLOSSA_REPO="${GLOSSA_REPO:-Glaicer/glossa}"
readonly GLOSSA_VERSION="${GLOSSA_VERSION:-latest}"
readonly GLOSSA_ASSET_NAME="${GLOSSA_ASSET_NAME:-glossa-linux-x86_64.tar.gz}"
readonly CHECKSUMS_ASSET_NAME="${CHECKSUMS_ASSET_NAME:-sha256sums.txt}"
readonly GLOSSA_RELEASE_BASE_URL="${GLOSSA_RELEASE_BASE_URL:-}"
readonly LOCAL_BIN_DIR="${HOME}/.local/bin"
readonly LOCAL_SHARE_DIR="${HOME}/.local/share/glossa"
readonly LOCAL_ASSETS_DIR="${LOCAL_SHARE_DIR}/assets"
readonly LOCAL_TRAY_DIR="${LOCAL_ASSETS_DIR}/tray"
readonly LOCAL_SOUND_DIR="${LOCAL_ASSETS_DIR}/sounds"
readonly LOCAL_VERSION_PATH="${LOCAL_SHARE_DIR}/VERSION"
readonly CONFIG_DIR="${HOME}/.config/glossa"
readonly CONFIG_PATH="${CONFIG_DIR}/config.toml"
readonly GLOSSA_ENV_PATH="${CONFIG_DIR}/glossa.env"
readonly SYSTEMD_USER_DIR="${HOME}/.config/systemd/user"
readonly GLOSSA_SERVICE_PATH="${SYSTEMD_USER_DIR}/glossa.service"
readonly DOTOOL_SERVICE_PATH="${SYSTEMD_USER_DIR}/dotool.service"
readonly INSTALLED_GLOSSA_PATH="${LOCAL_BIN_DIR}/glossa"
readonly INSTALLED_UPDATE_PATH="${LOCAL_BIN_DIR}/update.sh"
readonly INSTALLED_DOTOOL_PATH="/usr/local/bin/dotool"
readonly INSTALLED_DOTOOLC_PATH="/usr/local/bin/dotoolc"
readonly INSTALLED_DOTOOLD_PATH="/usr/local/bin/dotoold"
readonly INSTALLED_DOTOOL_RULES_PATH="/etc/udev/rules.d/80-dotool.rules"
readonly INSTALLED_DOTOOL_MODULES_LOAD_PATH="/etc/modules-load.d/dotool.conf"

tmpdir=""
glossa_bundle_dir=""
tty_fd=3
wl_copy_path=""
dotool_path=""
dotoold_path=""
dotoolc_path=""
glossa_path="${INSTALLED_GLOSSA_PATH}"
dotool_installed_this_run=0
dotool_requires_new_login=0
manual_provider_setup=0
created_or_updated_config=0
keep_existing_config=0
glossa_service_ready=1
missing_service_env_name=""
provider_kind="groq"
provider_base_url="https://api.groq.com/openai/v1"
provider_model="whisper-large-v3-turbo"
provider_api_key=""
input_backend="portal"
input_mode="push-to-talk"
ui_theme="dark"

cleanup() {
  if [[ -n "${tmpdir}" && -d "${tmpdir}" ]]; then
    rm -rf "${tmpdir}"
  fi
}
trap cleanup EXIT

detect_pkg_manager() {
  if command -v apt-get >/dev/null 2>&1; then
    echo "apt-get"
  elif command -v dnf >/dev/null 2>&1; then
    echo "dnf"
  elif command -v yum >/dev/null 2>&1; then
    echo "yum"
  else
    die "No supported package manager found (tried apt-get, dnf, yum)."
  fi
}

install_system_packages() {
  local pkg_manager
  pkg_manager="$(detect_pkg_manager)"

  case "${pkg_manager}" in
    apt-get)
      sudo apt-get update
      sudo apt-get install -y "$@"
      ;;
    dnf)
      sudo dnf install -y "$@"
      ;;
    yum)
      sudo yum install -y "$@"
      ;;
  esac
}

package_name_for_distro() {
  local debian_name="$1"
  local fedora_name="$2"
  local pkg_manager
  pkg_manager="$(detect_pkg_manager)"

  if [[ "${pkg_manager}" == "dnf" || "${pkg_manager}" == "yum" ]]; then
    printf '%s' "${fedora_name}"
  else
    printf '%s' "${debian_name}"
  fi
}

log() {
  printf '%s\n' "$*"
}

warn() {
  printf 'Warning: %s\n' "$*" >&2
}

die() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  local command_name="$1"
  command -v "${command_name}" >/dev/null 2>&1 || die "Required command '${command_name}' is not available."
}

open_tty() {
  [[ -r /dev/tty ]] || die "An interactive TTY is required to run this installer."
  exec {tty_fd}<> /dev/tty
}

prompt() {
  local message="$1"
  printf '%s' "${message}" >&${tty_fd}
}

read_tty() {
  local answer
  IFS= read -r answer <&${tty_fd} || true
  printf '%s' "${answer}"
}

normalize_answer() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

config_api_key_source() {
  if [[ ! -f "${CONFIG_PATH}" ]]; then
    return 0
  fi

  sed -n 's/^api_key = "\(.*\)"$/\1/p' "${CONFIG_PATH}" | head -n 1
}

env_file_contains_var() {
  local env_name="$1"
  [[ -f "${GLOSSA_ENV_PATH}" ]] && grep -Eq "^${env_name}=" "${GLOSSA_ENV_PATH}"
}

write_glossa_env_file() {
  local env_name="$1"
  local env_value="$2"

  mkdir -p "${CONFIG_DIR}"
  printf '%s="%s"\n' "${env_name}" "${env_value//\"/\\\"}" > "${GLOSSA_ENV_PATH}"
}

sync_glossa_service_environment() {
  local api_key_source
  local env_name
  local env_value

  glossa_service_ready=1
  missing_service_env_name=""
  api_key_source="$(config_api_key_source)"

  if [[ -z "${api_key_source}" || "${api_key_source}" != env:* ]]; then
    rm -f "${GLOSSA_ENV_PATH}"
    return 0
  fi

  env_name="${api_key_source#env:}"
  if [[ -z "${env_name}" ]]; then
    die "Config uses an invalid env-based provider.api_key source."
  fi

  env_value="${!env_name-}"
  if [[ -n "${env_value}" ]]; then
    write_glossa_env_file "${env_name}" "${env_value}"
    return 0
  fi

  if env_file_contains_var "${env_name}"; then
    return 0
  fi

  glossa_service_ready=0
  missing_service_env_name="${env_name}"
  warn "The current shell does not define ${env_name}. Glossa will be installed but not started until ${GLOSSA_ENV_PATH} provides it."
}

confirm_continue() {
  log "Glossa installer"
  log
  log "This script will:"
  log "- verify that you are running GNOME on Wayland"
  log "- install runtime dependencies such as wl-clipboard, libnotify, AppIndicator, libxdo, and dotool if missing"
  log "- download the latest Glossa release bundle from GitHub"
  log "- create ~/.config/glossa/config.toml and systemd user services"
  log

  while true; do
    prompt "Continue with the installation? [Y/n] "
    case "$(normalize_answer "$(read_tty)")" in
      "" | y | yes)
        return 0
        ;;
      n | no)
        log "Installation cancelled."
        exit 0
        ;;
      *)
        log "Please answer yes or no."
        ;;
    esac
  done
}

assert_not_root() {
  [[ "${EUID}" -ne 0 ]] || die "Run this installer as your regular user. The script will call sudo only when needed."
}

release_download_url() {
  local asset_name="$1"

  if [[ -n "${GLOSSA_RELEASE_BASE_URL}" ]]; then
    printf '%s/%s' "${GLOSSA_RELEASE_BASE_URL%/}" "${asset_name}"
    return 0
  fi

  if [[ "${GLOSSA_VERSION}" == "latest" ]]; then
    printf 'https://github.com/%s/releases/latest/download/%s' "${GLOSSA_REPO}" "${asset_name}"
  else
    printf 'https://github.com/%s/releases/download/%s/%s' "${GLOSSA_REPO}" "${GLOSSA_VERSION}" "${asset_name}"
  fi
}

download_asset() {
  local asset_name="$1"
  local destination="$2"
  local url
  url="$(release_download_url "${asset_name}")"

  log "Downloading ${asset_name}"
  if [[ "${url}" == https://* ]]; then
    curl -fL --retry 3 --retry-delay 1 --proto '=https' --tlsv1.2 -o "${destination}" "${url}"
  else
    curl -fL --retry 3 --retry-delay 1 -o "${destination}" "${url}"
  fi
}

ensure_tmpdir() {
  if [[ -z "${tmpdir}" ]]; then
    tmpdir="$(mktemp -d)"
  fi
}

verify_downloaded_assets() {
  local checksum_path="$1"
  shift
  local asset_name

  for asset_name in "$@"; do
    grep -F "${asset_name}" "${checksum_path}" >/dev/null 2>&1 \
      || die "Checksum file does not contain an entry for ${asset_name}."
  done

  (
    cd "${tmpdir}"
    sha256sum -c --ignore-missing "${checksum_path##*/}"
  ) || die "Checksum verification failed."
}

assert_x86_64() {
  local architecture
  architecture="$(uname -m)"

  [[ "${architecture}" == "x86_64" || "${architecture}" == "amd64" ]] \
    || die "Glossa currently supports x86_64 only. Detected '${architecture}'."
}

assert_gnome() {
  local desktop
  desktop="${XDG_CURRENT_DESKTOP:-${DESKTOP_SESSION:-}}"

  printf '%s' "${desktop}" | tr '[:lower:]' '[:upper:]' | grep -Fq "GNOME" \
    || die "Glossa currently supports GNOME only. Detected desktop session '${desktop:-unknown}'."
}

assert_wayland() {
  if [[ "${XDG_SESSION_TYPE:-}" == "wayland" || -n "${WAYLAND_DISPLAY:-}" ]]; then
    return 0
  fi

  die "Glossa currently supports Wayland sessions only. Installation aborted."
}

assert_runtime_prerequisites() {
  require_command curl
  require_command tar
  require_command sha256sum
  require_command systemctl
  require_command sudo
}

ensure_wl_copy() {
  if command -v wl-copy >/dev/null 2>&1 && command -v wl-paste >/dev/null 2>&1; then
    wl_copy_path="$(command -v wl-copy)"
  else
    log "wl-clipboard was not found. Installing wl-clipboard."
    install_system_packages wl-clipboard
    command -v wl-copy >/dev/null 2>&1 || die "wl-copy is still unavailable after installing wl-clipboard."
    command -v wl-paste >/dev/null 2>&1 || die "wl-paste is still unavailable after installing wl-clipboard."
    wl_copy_path="$(command -v wl-copy)"
  fi

  "${wl_copy_path}" --version >/dev/null 2>&1 \
    || die "wl-copy is installed at ${wl_copy_path}, but 'wl-copy --version' failed."
}

ensure_notify_send() {
  if command -v notify-send >/dev/null 2>&1; then
    notify_send_path="$(command -v notify-send)"
  else
    log "notify-send was not found. Installing libnotify."
    install_system_packages "$(package_name_for_distro libnotify-bin libnotify)"
    command -v notify-send >/dev/null 2>&1 || die "notify-send is still unavailable after installing libnotify."
    notify_send_path="$(command -v notify-send)"
  fi

  "${notify_send_path}" --version >/dev/null 2>&1 \
    || die "notify-send is installed at ${notify_send_path}, but 'notify-send --version' failed."
}

ensure_appindicator_runtime() {
  if ldconfig -p 2>/dev/null | grep -Eq 'lib(ayatana-)?appindicator3\.so\.1'; then
    return 0
  fi

  log "AppIndicator runtime library was not found. Installing it."
  install_system_packages "$(package_name_for_distro libayatana-appindicator3-1 libayatana-appindicator-gtk3)"

  ldconfig -p 2>/dev/null | grep -Eq 'lib(ayatana-)?appindicator3\.so\.1' \
    || die "AppIndicator runtime library is still unavailable after installing it."
}

ensure_libxdo3() {
  if ldconfig -p 2>/dev/null | grep -q 'libxdo.so.3'; then
    return 0
  fi

  log "libxdo3 was not found. Installing it."
  install_system_packages "$(package_name_for_distro libxdo3 libxdo)"

  ldconfig -p 2>/dev/null | grep -q 'libxdo.so.3' \
    || die "libxdo3 is still unavailable after installing it."
}

dotool_bundle_ready() {
  command -v dotool >/dev/null 2>&1 \
    && command -v dotoold >/dev/null 2>&1 \
    && command -v dotoolc >/dev/null 2>&1
}

install_dotool_bundle_from_glossa_bundle() {
  local bundle_dir="$1"
  local dotool_source="${bundle_dir}/dotool/dotool"
  local dotoolc_source="${bundle_dir}/dotool/dotoolc"
  local dotoold_source="${bundle_dir}/dotool/dotoold"

  [[ -f "${dotool_source}" ]] || die "The Glossa release bundle does not contain 'dotool/dotool'."
  [[ -f "${dotoolc_source}" ]] || die "The Glossa release bundle does not contain 'dotool/dotoolc'."
  [[ -f "${dotoold_source}" ]] || die "The Glossa release bundle does not contain 'dotool/dotoold'."

  log "Installing dotool binaries."
  sudo install -Dm755 "${dotool_source}" "${INSTALLED_DOTOOL_PATH}"
  sudo install -Dm755 "${dotoolc_source}" "${INSTALLED_DOTOOLC_PATH}"
  sudo install -Dm755 "${dotoold_source}" "${INSTALLED_DOTOOLD_PATH}"
  dotool_installed_this_run=1
}

install_dotool_uinput_access() {
  local rules_source="$1"

  [[ -f "${rules_source}" ]] || die "The Glossa release bundle does not contain 'dotool/80-dotool.rules'."

  log "Configuring dotool uinput access."
  sudo install -Dm644 "${rules_source}" "${INSTALLED_DOTOOL_RULES_PATH}"
  sudo groupadd -f input
  if ! id -nG "$(id -un)" | tr ' ' '\n' | grep -qx input; then
    dotool_requires_new_login=1
  fi
  sudo usermod -a -G input "$(id -un)"
  printf 'uinput\n' | sudo tee "${INSTALLED_DOTOOL_MODULES_LOAD_PATH}" >/dev/null
  sudo modprobe uinput || die "Failed to load the uinput kernel module required by dotool."
  sudo udevadm control --reload
  sudo udevadm trigger
}

ensure_dotool_bundle() {
  local rules_source
  [[ -n "${glossa_bundle_dir}" ]] || die "The Glossa release bundle was not extracted."
  rules_source="${glossa_bundle_dir}/dotool/80-dotool.rules"

  if dotool_bundle_ready; then
    dotool_path="$(command -v dotool)"
    dotoold_path="$(command -v dotoold)"
    dotoolc_path="$(command -v dotoolc)"
  else
    install_dotool_bundle_from_glossa_bundle "${glossa_bundle_dir}"
    dotool_path="${INSTALLED_DOTOOL_PATH}"
    dotoold_path="${INSTALLED_DOTOOLD_PATH}"
    dotoolc_path="${INSTALLED_DOTOOLC_PATH}"
  fi

  install_dotool_uinput_access "${rules_source}"

  "${dotool_path}" --version >/dev/null 2>&1 \
    || die "dotool is installed at ${dotool_path}, but 'dotool --version' failed."
}

extract_glossa_bundle() {
  local tarball_path="$1"

  glossa_bundle_dir="${tmpdir}/glossa"
  rm -rf "${glossa_bundle_dir}"
  mkdir -p "${glossa_bundle_dir}"
  tar -xzf "${tarball_path}" -C "${glossa_bundle_dir}"
}

install_glossa_bundle() {
  local bundle_dir="$1"
  local glossa_source="${bundle_dir}/glossa"
  local update_source="${bundle_dir}/update.sh"
  local tray_source="${bundle_dir}/assets/tray"
  local sound_source="${bundle_dir}/assets/sounds"
  local version_source="${bundle_dir}/VERSION"

  [[ -f "${glossa_source}" ]] || die "The Glossa release bundle does not contain the 'glossa' binary."
  [[ -f "${update_source}" ]] || die "The Glossa release bundle does not contain 'update.sh'."
  [[ -d "${tray_source}" ]] || die "The Glossa release bundle does not contain tray icons under assets/tray."
  [[ -d "${sound_source}" ]] || die "The Glossa release bundle does not contain cue sounds under assets/sounds."
  [[ -f "${version_source}" ]] || die "The Glossa release bundle does not contain VERSION."

  mkdir -p "${LOCAL_BIN_DIR}" "${LOCAL_TRAY_DIR}" "${LOCAL_SOUND_DIR}" "${LOCAL_SHARE_DIR}"
  install -m755 "${glossa_source}" "${INSTALLED_GLOSSA_PATH}"
  install -m755 "${update_source}" "${INSTALLED_UPDATE_PATH}"
  install -m644 "${tray_source}/"* "${LOCAL_TRAY_DIR}/"
  install -m644 "${sound_source}/"* "${LOCAL_SOUND_DIR}/"
  install -m644 "${version_source}" "${LOCAL_VERSION_PATH}"

  glossa_path="${INSTALLED_GLOSSA_PATH}"
}

download_and_extract_glossa_bundle() {
  ensure_tmpdir
  download_asset "${GLOSSA_ASSET_NAME}" "${tmpdir}/${GLOSSA_ASSET_NAME}"
  download_asset "${CHECKSUMS_ASSET_NAME}" "${tmpdir}/${CHECKSUMS_ASSET_NAME}"
  verify_downloaded_assets "${tmpdir}/${CHECKSUMS_ASSET_NAME}" "${GLOSSA_ASSET_NAME}"
  extract_glossa_bundle "${tmpdir}/${GLOSSA_ASSET_NAME}"
}

download_and_install_glossa() {
  download_and_extract_glossa_bundle
  ensure_dotool_bundle
  install_glossa_bundle "${glossa_bundle_dir}"
  "${glossa_path}" --help >/dev/null 2>&1 \
    || die "The installed Glossa binary could not start. Ensure required runtime libraries such as libxdo3 are installed."
}

prompt_backend() {
  while true; do
    prompt $'Would you like to register hotkeys globally on first start?\n\n1) Yes\n2) No\n\nSelection [1]: '
    case "$(normalize_answer "$(read_tty)")" in
      "" | 1 | "1)" | y | yes)
        input_backend="portal"
        return 0
        ;;
      2 | "2)" | n | no)
        input_backend="none"
        return 0
        ;;
      *)
        log "Please choose 1 or 2."
        ;;
    esac
  done
}

prompt_input_mode() {
  while true; do
    prompt $'Do you prefer push-to-talk mode or toggle mode?\n\n1) Push-to-talk\n2) Toggle\n\nSelection: '
    case "$(normalize_answer "$(read_tty)")" in
      1 | "1)" | push-to-talk | pushtotalk)
        input_mode="push-to-talk"
        return 0
        ;;
      2 | "2)" | toggle)
        input_mode="toggle"
        return 0
        ;;
      *)
        log "Please choose 1 or 2."
        ;;
    esac
  done
}

prompt_provider() {
  while true; do
    prompt $'What API provider would you like to use?\n\n1) Groq (default)\n2) OpenAI\n3) Other / self-hosted\n\nSelection [1]: '
    case "$(normalize_answer "$(read_tty)")" in
      "" | 1 | "1)" | groq)
        provider_kind="groq"
        provider_base_url="https://api.groq.com/openai/v1"
        manual_provider_setup=0
        return 0
        ;;
      2 | "2)" | openai)
        provider_kind="openai"
        provider_base_url="https://api.openai.com/v1"
        manual_provider_setup=0
        return 0
        ;;
      3 | "3)" | other | self-hosted | selfhosted)
        provider_kind="openai-compatible"
        provider_base_url="https://example.com/openai/v1"
        provider_model="example-model"
        provider_api_key="SET_YOUR_API_KEY"
        manual_provider_setup=1
        return 0
        ;;
      *)
        log "Please choose 1, 2, or 3."
        ;;
    esac
  done
}

prompt_model() {
  if [[ "${provider_kind}" == "groq" ]]; then
    while true; do
      prompt $'What model would you like to use?\n\n1) Whisper Large V3 Turbo (default)\n2) Whisper Large V3\n\nSelection [1]: '
      case "$(normalize_answer "$(read_tty)")" in
        "" | 1 | "1)" | whisper-large-v3-turbo | turbo)
          provider_model="whisper-large-v3-turbo"
          return 0
          ;;
        2 | "2)" | whisper-large-v3 | large)
          provider_model="whisper-large-v3"
          return 0
          ;;
        *)
          log "Please choose 1 or 2."
          ;;
      esac
    done
  fi

  while true; do
    prompt $'What model would you like to use?\n\n1) Whisper\n2) gpt-4o-transcribe\n3) gpt-4o-mini-transcribe\n\nSelection [1]: '
    case "$(normalize_answer "$(read_tty)")" in
      "" | 1 | "1)" | whisper | whisper-1)
        provider_model="whisper-1"
        return 0
        ;;
      2 | "2)" | gpt-4o-transcribe)
        provider_model="gpt-4o-transcribe"
        return 0
        ;;
      3 | "3)" | gpt-4o-mini-transcribe)
        provider_model="gpt-4o-mini-transcribe"
        return 0
        ;;
      *)
        log "Please choose 1, 2, or 3."
        ;;
    esac
  done
}

prompt_api_key() {
  while true; do
    prompt "Insert your API key (you can also enter env:VARIABLE_NAME): "
    provider_api_key="$(read_tty)"

    if [[ -n "${provider_api_key}" ]]; then
      return 0
    fi

    log "The API key cannot be empty."
  done
}

prompt_theme() {
  while true; do
    prompt $'Is your system panel dark or light? This sets the tray icons.\n\n1) Dark (default)\n2) Light\n\nSelection [1]: '
    case "$(normalize_answer "$(read_tty)")" in
      "" | 1 | "1)" | dark)
        ui_theme="dark"
        return 0
        ;;
      2 | "2)" | light)
        ui_theme="light"
        return 0
        ;;
      *)
        log "Please choose 1 or 2."
        ;;
    esac
  done
}

configure_new_install() {
  prompt_backend

  if [[ "${input_backend}" == "portal" ]]; then
    prompt_input_mode
  fi

  prompt_provider

  if (( manual_provider_setup == 0 )); then
    prompt_model
    prompt_api_key
  else
    log "Self-hosted providers need manual setup. The installer will write placeholder values to ${CONFIG_PATH}."
  fi

  prompt_theme
}

prompt_existing_config() {
  if [[ ! -f "${CONFIG_PATH}" ]]; then
    return 0
  fi

  while true; do
    prompt "A config already exists at ${CONFIG_PATH}. Keep it? [Y/n] "
    case "$(normalize_answer "$(read_tty)")" in
      "" | y | yes)
        keep_existing_config=1
        return 0
        ;;
      n | no)
        keep_existing_config=0
        return 0
        ;;
      *)
        log "Please answer yes or no."
        ;;
    esac
  done
}

backup_existing_config() {
  if [[ -f "${CONFIG_PATH}" ]]; then
    local backup_path
    backup_path="${CONFIG_PATH}.$(date +%Y%m%d-%H%M%S).bak"
    cp "${CONFIG_PATH}" "${backup_path}"
    log "Backed up the previous config to ${backup_path}"
  fi
}

write_config_file() {
  mkdir -p "${CONFIG_DIR}"

  cat > "${CONFIG_PATH}" <<EOF
[input]
backend = "${input_backend}"
mode = "${input_mode}"

[control]
enable_cli = true
socket_path = "auto"

[provider]
kind = "${provider_kind}"
base_url = "${provider_base_url}"
model = "${provider_model}"
api_key = "${provider_api_key}"

[LLM]
enabled = false
base_url = ""
model = ""
api_key = ""

[audio]
work_dir = "auto"
format = "wav"
sample_rate_hz = 16000
channels = 1
trim_silence = true
trim_threshold = 500
min_duration_ms = 150
max_duration_sec = 120
persist_audio = false
latency_mode = "balanced"
keepalive_after_stop_seconds = 60

[paste]
mode = "shift-insert"
append_space = false
clipboard_command = "${wl_copy_path}"
type_command = "${dotoolc_path}"

[ui]
tray = true
theme = "${ui_theme}"
idle_icon = "${LOCAL_TRAY_DIR}/idle.png"
recording_icon = "${LOCAL_TRAY_DIR}/recording.png"
processing_icon = "${LOCAL_TRAY_DIR}/processing.png"
idle_dark_icon = "${LOCAL_TRAY_DIR}/idle_dark.png"
recording_dark_icon = "${LOCAL_TRAY_DIR}/recording_dark.png"
processing_dark_icon = "${LOCAL_TRAY_DIR}/processing_dark.png"
start_sound = "${LOCAL_SOUND_DIR}/start.wav"
stop_sound = "${LOCAL_SOUND_DIR}/stop.wav"

[logging]
level = "info"
journal = true
file = false
EOF

  created_or_updated_config=1

  if (( manual_provider_setup == 1 )); then
    cat <<EOF

The generated config contains placeholder provider values:
- base_url = "https://example.com/openai/v1"
- model = "example-model"
- api_key = "SET_YOUR_API_KEY"

Edit ${CONFIG_PATH} before starting Glossa.
EOF
  fi
}

maybe_generate_config() {
  prompt_existing_config

  if (( keep_existing_config == 1 )); then
    log "Keeping the existing config at ${CONFIG_PATH}"
  else
    configure_new_install
    backup_existing_config
    write_config_file
  fi

  sync_glossa_service_environment
}

write_dotool_service() {
  mkdir -p "${SYSTEMD_USER_DIR}"

  cat > "${DOTOOL_SERVICE_PATH}" <<EOF
[Unit]
Description=dotool daemon
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=simple
ExecStart=${dotoold_path}
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
EOF
}

write_glossa_service() {
  mkdir -p "${SYSTEMD_USER_DIR}"

  cat > "${GLOSSA_SERVICE_PATH}" <<'EOF'
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
}

enable_services() {
  systemctl --user daemon-reload

  if (( dotool_installed_this_run == 1 || dotool_requires_new_login == 1 )); then
    systemctl --user enable dotool.service >/dev/null
  else
    systemctl --user enable --now dotool.service >/dev/null
  fi

  if (( manual_provider_setup == 1 )); then
    return 0
  fi

  if (( glossa_service_ready == 0 )); then
    systemctl --user stop glossa.service >/dev/null 2>&1 || true
    systemctl --user enable glossa.service >/dev/null
    return 0
  fi

  if (( dotool_installed_this_run == 1 || dotool_requires_new_login == 1 )); then
    systemctl --user enable glossa.service >/dev/null
  else
    systemctl --user enable --now glossa.service >/dev/null
  fi
}

print_service_status() {
  local service_name="$1"
  local enabled_state
  local active_state

  enabled_state="$(systemctl --user is-enabled "${service_name}" 2>/dev/null || true)"
  active_state="$(systemctl --user is-active "${service_name}" 2>/dev/null || true)"

  log "${service_name}: enabled=${enabled_state:-unknown}, active=${active_state:-unknown}"
}

run_doctor() {
  if [[ ! -x "${glossa_path}" || ! -f "${CONFIG_PATH}" ]]; then
    return 0
  fi

  log
  log "Glossa diagnostics:"
  "${glossa_path}" --config "${CONFIG_PATH}" doctor || true
}

print_final_summary() {
  log
  log "Installation complete."
  log "- Glossa binary: ${glossa_path}"
  log "- Updater script: ${INSTALLED_UPDATE_PATH}"
  log "- Config: ${CONFIG_PATH}"
  log "- Assets: ${LOCAL_SHARE_DIR}"
  log
  print_service_status dotool.service
  print_service_status glossa.service
  run_doctor
  log
  log "You can change the configuration at ${CONFIG_PATH}"
  log "To enable AI cleaner and enhancer for the transcibed text visit Settings and configure LLM Enhancer model, provider and API key"

  if (( manual_provider_setup == 1 )); then
    log "Edit the provider settings first, then start Glossa with: systemctl --user start glossa.service"
  fi

  if (( glossa_service_ready == 0 )); then
    log "Create ${GLOSSA_ENV_PATH} with ${missing_service_env_name}=... or rerun the installer with ${missing_service_env_name} exported, then start Glossa with: systemctl --user start glossa.service"
  fi

  if (( dotool_installed_this_run == 1 || dotool_requires_new_login == 1 )); then
    log "dotool permissions changed during this run. Log out and back in, or reboot, before expecting paste to work."
  fi
}

main() {
  open_tty
  assert_not_root
  confirm_continue
  assert_runtime_prerequisites
  assert_x86_64
  assert_gnome
  assert_wayland
  systemctl --user --version >/dev/null 2>&1 || die "systemctl --user is not available in this session."
  ensure_wl_copy
  ensure_notify_send
  ensure_appindicator_runtime
  ensure_libxdo3
  download_and_install_glossa
  maybe_generate_config
  write_dotool_service
  write_glossa_service
  enable_services
  print_final_summary
}

if [[ -n "${BASH_SOURCE[0]:-}" && "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
