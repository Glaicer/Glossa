#!/usr/bin/env bash
set -euo pipefail

readonly LOCAL_BIN_DIR="${HOME}/.local/bin"
readonly LOCAL_SHARE_DIR="${HOME}/.local/share/glossa"
readonly LOCAL_ASSETS_DIR="${LOCAL_SHARE_DIR}/assets"
readonly LOCAL_TRAY_DIR="${LOCAL_ASSETS_DIR}/tray"
readonly LOCAL_SOUND_DIR="${LOCAL_ASSETS_DIR}/sounds"
readonly CONFIG_DIR="${HOME}/.config/glossa"
readonly CONFIG_PATH="${CONFIG_DIR}/config.toml"
readonly SYSTEMD_USER_DIR="${HOME}/.config/systemd/user"
readonly GLOSSA_SERVICE_PATH="${SYSTEMD_USER_DIR}/glossa.service"
readonly DOTOOL_SERVICE_PATH="${SYSTEMD_USER_DIR}/dotool.service"
readonly INSTALLED_GLOSSA_PATH="${LOCAL_BIN_DIR}/glossa"
readonly INSTALLED_DOTOOL_PATH="/usr/local/bin/dotool"
readonly INSTALLED_DOTOOLC_PATH="/usr/local/bin/dotoolc"
readonly INSTALLED_DOTOOLD_PATH="/usr/local/bin/dotoold"
readonly INSTALLED_DOTOOL_RULES_PATH="/etc/udev/rules.d/80-dotool.rules"
readonly INSTALLED_DOTOOL_MODULES_LOAD_PATH="/etc/modules-load.d/dotool.conf"

tty_fd=3
remove_wl_clipboard=0
remove_libnotify_libxdo=0
remove_dotool=0
config_result="absent"

remove_system_packages() {
  local pkg_manager
  if command -v apt-get >/dev/null 2>&1; then
    pkg_manager="apt-get"
  elif command -v dnf >/dev/null 2>&1; then
    pkg_manager="dnf"
  elif command -v yum >/dev/null 2>&1; then
    pkg_manager="yum"
  else
    warn "No supported package manager found; skipping package removal."
    return 1
  fi

  case "${pkg_manager}" in
    apt-get)
      sudo apt-get remove -y "$@"
      ;;
    dnf)
      sudo dnf remove -y "$@"
      ;;
    yum)
      sudo yum remove -y "$@"
      ;;
  esac
}

log() {
  printf '%s\n' "$*"
}

die() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

open_tty() {
  [[ -r /dev/tty ]] || die "An interactive TTY is required to run this uninstaller."
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

confirm_yes_no() {
  local message="$1"
  local default_answer="${2:-yes}"
  local answer

  while true; do
    prompt "${message}"
    answer="$(normalize_answer "$(read_tty)")"

    case "${answer}" in
      "")
        [[ "${default_answer}" == "yes" ]] && return 0
        return 1
        ;;
      y | yes)
        return 0
        ;;
      n | no)
        return 1
        ;;
      *)
        log "Please answer yes or no."
        ;;
    esac
  done
}

confirm_continue() {
  log "Glossa uninstaller"
  log
  log "This script will:"
  log "- stop and remove the Glossa user services"
  log "- remove the Glossa binary, bundled assets, and generated config"
  log "- optionally remove wl-clipboard, libnotify, and dotool"
  log

  if ! confirm_yes_no "Continue with the uninstall? [Y/n] " "yes"; then
    log "Uninstall cancelled."
    exit 0
  fi
}

prompt_optional_packages() {
  if confirm_yes_no "Uninstall wl-clipboard? [y/N] " "no"; then
    remove_wl_clipboard=1
  fi

  if confirm_yes_no "Uninstall libnotify and libxdo? [y/N] " "no"; then
    remove_libnotify_libxdo=1
  fi

  if confirm_yes_no "Uninstall dotool? [y/N] " "no"; then
    remove_dotool=1
  fi
}

assert_not_root() {
  [[ "${EUID}" -ne 0 ]] || die "Run this uninstaller as your regular user. The script will call sudo only when needed."
}

require_command() {
  local command_name="$1"
  command -v "${command_name}" >/dev/null 2>&1 || die "Required command '${command_name}' is not available."
}

assert_runtime_prerequisites() {
  require_command systemctl
  require_command sudo
}

remove_file_if_exists() {
  local path="$1"

  if [[ -e "${path}" || -L "${path}" ]]; then
    rm -f "${path}"
    log "Removed ${path}"
  fi
}

remove_dir_if_empty() {
  local path="$1"

  if [[ -d "${path}" ]] && rmdir "${path}" 2>/dev/null; then
    log "Removed empty directory ${path}"
  fi
}

config_is_installer_managed() {
  [[ -f "${CONFIG_PATH}" ]] || return 1

  grep -Fq "idle_icon = \"${LOCAL_TRAY_DIR}/idle.png\"" "${CONFIG_PATH}" \
    && grep -Fq "start_sound = \"${LOCAL_SOUND_DIR}/start.wav\"" "${CONFIG_PATH}" \
    && grep -Fq "type_command = \"/usr/local/bin/dotool\"" "${CONFIG_PATH}"
}

restore_config_backup_if_available() {
  local latest_backup

  latest_backup="$(find "${CONFIG_DIR}" -maxdepth 1 -type f -name 'config.toml.*.bak' -print 2>/dev/null | sort | tail -n 1)"

  if [[ -n "${latest_backup}" ]]; then
    mv "${latest_backup}" "${CONFIG_PATH}"
    log "Restored previous config from ${latest_backup}"
    config_result="restored"
    return 0
  fi

  if config_is_installer_managed; then
    remove_file_if_exists "${CONFIG_PATH}"
    config_result="removed"
  elif [[ -f "${CONFIG_PATH}" ]]; then
    log "Keeping ${CONFIG_PATH} because it does not look like an install.sh-generated config."
    config_result="kept"
  else
    config_result="absent"
  fi
}

disable_and_remove_service() {
  local service_name="$1"
  local service_path="$2"

  systemctl --user disable --now "${service_name}" >/dev/null 2>&1 || true
  remove_file_if_exists "${service_path}"
}

remove_glossa_installation() {
  disable_and_remove_service "glossa.service" "${GLOSSA_SERVICE_PATH}"

  remove_file_if_exists "${INSTALLED_GLOSSA_PATH}"

  if [[ -d "${LOCAL_TRAY_DIR}" ]]; then
    rm -rf "${LOCAL_TRAY_DIR}"
    log "Removed ${LOCAL_TRAY_DIR}"
  fi

  if [[ -d "${LOCAL_SOUND_DIR}" ]]; then
    rm -rf "${LOCAL_SOUND_DIR}"
    log "Removed ${LOCAL_SOUND_DIR}"
  fi

  restore_config_backup_if_available

  remove_dir_if_empty "${LOCAL_ASSETS_DIR}"
  remove_dir_if_empty "${LOCAL_SHARE_DIR}"
  remove_dir_if_empty "${CONFIG_DIR}"
}

remove_dotool_installation() {
  disable_and_remove_service "dotool.service" "${DOTOOL_SERVICE_PATH}"

  if command -v wl-copy >/dev/null 2>&1 && (( remove_wl_clipboard == 1 )); then
    remove_system_packages wl-clipboard || true
  fi

  if (( remove_libnotify_libxdo == 1 )); then
    remove_system_packages libnotify libxdo || true
  fi

  if (( remove_dotool == 1 )); then
    if [[ -x "${INSTALLED_DOTOOL_PATH}" || -e "${INSTALLED_DOTOOLC_PATH}" || -e "${INSTALLED_DOTOOLD_PATH}" ]]; then
      sudo rm -f "${INSTALLED_DOTOOL_PATH}" "${INSTALLED_DOTOOLC_PATH}" "${INSTALLED_DOTOOLD_PATH}"
      log "Removed dotool binaries from /usr/local/bin"
    fi

    if [[ -e "${INSTALLED_DOTOOL_RULES_PATH}" ]]; then
      sudo rm -f "${INSTALLED_DOTOOL_RULES_PATH}"
      log "Removed ${INSTALLED_DOTOOL_RULES_PATH}"
      sudo udevadm control --reload
      sudo udevadm trigger
    fi

    if [[ -e "${INSTALLED_DOTOOL_MODULES_LOAD_PATH}" ]]; then
      sudo rm -f "${INSTALLED_DOTOOL_MODULES_LOAD_PATH}"
      log "Removed ${INSTALLED_DOTOOL_MODULES_LOAD_PATH}"
    fi

    if getent group input >/dev/null 2>&1; then
      sudo gpasswd -d "$(id -un)" input >/dev/null 2>&1 || true
    fi
  fi
}

reload_systemd() {
  systemctl --user daemon-reload
  systemctl --user reset-failed >/dev/null 2>&1 || true
  remove_dir_if_empty "${SYSTEMD_USER_DIR}"
}

print_summary() {
  log
  log "Uninstall complete."
  log "- Removed Glossa user services, binary, and bundled assets"

  case "${config_result}" in
    restored)
      log "- Restored config at ${CONFIG_PATH}"
      ;;
    removed)
      log "- Removed generated config at ${CONFIG_PATH}"
      ;;
    kept)
      log "- Kept existing config at ${CONFIG_PATH}"
      ;;
    *)
      log "- No config change was needed at ${CONFIG_PATH}"
      ;;
  esac

  if (( remove_wl_clipboard == 1 )); then
    log "- Requested wl-clipboard removal"
  fi

  if (( remove_libnotify_libxdo == 1 )); then
    log "- Requested libnotify and libxdo removal"
  fi

  if (( remove_dotool == 1 )); then
    log "- Requested dotool removal"
  fi
}

main() {
  open_tty
  assert_not_root
  confirm_continue
  prompt_optional_packages
  assert_runtime_prerequisites
  remove_glossa_installation
  remove_dotool_installation
  reload_systemd
  print_summary
}

if [[ -n "${BASH_SOURCE[0]:-}" && "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
