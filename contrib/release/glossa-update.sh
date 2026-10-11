#!/usr/bin/env bash
set -euo pipefail

readonly GLOSSA_REPO="${GLOSSA_REPO:-Glaicer/glossa}"
readonly GLOSSA_VERSION="latest"
readonly GLOSSA_ASSET_NAME="${GLOSSA_ASSET_NAME:-glossa-linux-x86_64.tar.gz}"
readonly CHECKSUMS_ASSET_NAME="${CHECKSUMS_ASSET_NAME:-sha256sums.txt}"
readonly GLOSSA_RELEASE_BASE_URL="${GLOSSA_RELEASE_BASE_URL:-}"
readonly RELEASE_VERSION="__GLOSSA_RELEASE_VERSION__"
readonly LOCAL_BIN_DIR="${HOME}/.local/bin"
readonly LOCAL_SHARE_DIR="${HOME}/.local/share/glossa"
readonly LOCAL_ASSETS_DIR="${LOCAL_SHARE_DIR}/assets"
readonly LOCAL_TRAY_DIR="${LOCAL_ASSETS_DIR}/tray"
readonly LOCAL_SOUND_DIR="${LOCAL_ASSETS_DIR}/sounds"
readonly INSTALLED_GLOSSA_PATH="${LOCAL_BIN_DIR}/glossa"
readonly INSTALLED_VERSION_PATH="${LOCAL_SHARE_DIR}/VERSION"
readonly GLOSSA_SERVICE_NAME="glossa.service"

tmpdir=""
glossa_bundle_dir=""
command_name="install"

cleanup() {
  if [[ -n "${tmpdir}" && -d "${tmpdir}" ]]; then
    rm -rf "${tmpdir}"
  fi
}
trap cleanup EXIT

log() {
  printf '%s\n' "$*"
}

die() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

emit_result() {
  local status="$1"
  local version="$2"

  printf 'GLOSSA_UPDATE_STATUS=%s\n' "${status}"
  printf 'GLOSSA_UPDATE_VERSION=%s\n' "${version}"
}

require_command() {
  local command_name="$1"
  command -v "${command_name}" >/dev/null 2>&1 || die "Required command '${command_name}' is not available."
}

release_download_url() {
  local asset_name="$1"

  if [[ -n "${GLOSSA_RELEASE_BASE_URL}" ]]; then
    printf '%s/%s' "${GLOSSA_RELEASE_BASE_URL%/}" "${asset_name}"
    return 0
  fi

  printf 'https://github.com/%s/releases/%s/download/%s' "${GLOSSA_REPO}" "${GLOSSA_VERSION}" "${asset_name}"
}

download_asset() {
  local asset_name="$1"
  local destination="$2"

  log "Downloading ${asset_name}"
  wget -qO "${destination}" "$(release_download_url "${asset_name}")"
}

ensure_tmpdir() {
  if [[ -z "${tmpdir}" ]]; then
    tmpdir="$(mktemp -d)"
  fi
}

assert_installed_layout() {
  [[ -x "${INSTALLED_GLOSSA_PATH}" ]] || die "Glossa is not installed at ${INSTALLED_GLOSSA_PATH}."
}

installed_version() {
  local version_output

  if [[ -f "${INSTALLED_VERSION_PATH}" ]]; then
    IFS= read -r version_output < "${INSTALLED_VERSION_PATH}" || true
    printf '%s\n' "${version_output}"
    return 0
  fi

  version_output="$("${INSTALLED_GLOSSA_PATH}" --version 2>/dev/null || true)"

  if [[ -z "${version_output}" ]]; then
    printf 'unknown\n'
    return 0
  fi

  printf '%s\n' "${version_output##* }"
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

extract_glossa_bundle() {
  local tarball_path="$1"

  glossa_bundle_dir="${tmpdir}/glossa"
  rm -rf "${glossa_bundle_dir}"
  mkdir -p "${glossa_bundle_dir}"
  tar -xzf "${tarball_path}" -C "${glossa_bundle_dir}"
}

install_directory_files() {
  local source_dir="$1"
  local destination_dir="$2"
  local files=()

  mkdir -p "${destination_dir}"

  shopt -s nullglob
  files=("${source_dir}/"*)
  shopt -u nullglob

  ((${#files[@]} > 0)) || die "The release bundle does not contain files under ${source_dir}."
  install -m644 "${files[@]}" "${destination_dir}/"
}

install_glossa_bundle() {
  local bundle_dir="$1"
  local glossa_source="${bundle_dir}/glossa"
  local tray_source="${bundle_dir}/assets/tray"
  local sound_source="${bundle_dir}/assets/sounds"
  local version_source="${bundle_dir}/VERSION"

  [[ -f "${glossa_source}" ]] || die "The Glossa release bundle does not contain the 'glossa' binary."
  [[ -d "${tray_source}" ]] || die "The Glossa release bundle does not contain tray icons under assets/tray."
  [[ -d "${sound_source}" ]] || die "The Glossa release bundle does not contain cue sounds under assets/sounds."
  [[ -f "${version_source}" ]] || die "The Glossa release bundle does not contain VERSION."

  mkdir -p "${LOCAL_BIN_DIR}" "${LOCAL_TRAY_DIR}" "${LOCAL_SOUND_DIR}" "${LOCAL_SHARE_DIR}"
  install -m755 "${glossa_source}" "${INSTALLED_GLOSSA_PATH}"
  install_directory_files "${tray_source}" "${LOCAL_TRAY_DIR}"
  install_directory_files "${sound_source}" "${LOCAL_SOUND_DIR}"
  install -m644 "${version_source}" "${INSTALLED_VERSION_PATH}"

  "${INSTALLED_GLOSSA_PATH}" --help >/dev/null 2>&1 \
    || die "The installed Glossa binary could not start. Ensure required runtime libraries such as libxdo3 are installed."
}

download_and_extract_glossa_bundle() {
  ensure_tmpdir
  download_asset "${GLOSSA_ASSET_NAME}" "${tmpdir}/${GLOSSA_ASSET_NAME}"
  download_asset "${CHECKSUMS_ASSET_NAME}" "${tmpdir}/${CHECKSUMS_ASSET_NAME}"
  verify_downloaded_assets "${tmpdir}/${CHECKSUMS_ASSET_NAME}" "${GLOSSA_ASSET_NAME}"
  extract_glossa_bundle "${tmpdir}/${GLOSSA_ASSET_NAME}"
}

restart_service() {
  log "Restarting ${GLOSSA_SERVICE_NAME}"
  systemctl --user restart "${GLOSSA_SERVICE_NAME}"
}

parse_args() {
  command_name="${1:-install}"

  case "${command_name}" in
    check | install)
      ;;
    *)
      die "Unsupported updater command '${command_name}'. Expected 'check' or 'install'."
      ;;
  esac
}

main() {
  require_command install
  require_command mktemp
  require_command sha256sum
  require_command systemctl
  require_command tar
  require_command wget

  parse_args "${1:-}"
  assert_installed_layout

  local current_version
  current_version="$(installed_version)"

  if [[ "${current_version}" == "${RELEASE_VERSION}" ]]; then
    log "Glossa ${RELEASE_VERSION} is already installed."
    emit_result "up-to-date" "${RELEASE_VERSION}"
    exit 0
  fi

  if [[ "${command_name}" == "check" ]]; then
    emit_result "available" "${RELEASE_VERSION}"
    exit 0
  fi

  log "Updating Glossa from ${current_version} to ${RELEASE_VERSION}"
  download_and_extract_glossa_bundle
  install_glossa_bundle "${glossa_bundle_dir}"
  restart_service
  log "Update complete."
  emit_result "updated" "${RELEASE_VERSION}"
}

if [[ -n "${BASH_SOURCE[0]:-}" && "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
