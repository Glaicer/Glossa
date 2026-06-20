#!/usr/bin/env bash
set -euo pipefail

readonly GLOSSA_REPO="${GLOSSA_REPO:-Glaicer/glossa}"
readonly GLOSSA_VERSION="latest"
readonly UPDATER_ASSET_NAME="${UPDATER_ASSET_NAME:-glossa-update.sh}"
readonly CHECKSUMS_ASSET_NAME="${CHECKSUMS_ASSET_NAME:-sha256sums.txt}"
readonly GLOSSA_RELEASE_BASE_URL="${GLOSSA_RELEASE_BASE_URL:-}"

tmpdir=""

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

  if command -v curl >/dev/null 2>&1; then
    curl -fL --retry 3 --retry-delay 1 \
      -o "${destination}" "$(release_download_url "${asset_name}")"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "${destination}" "$(release_download_url "${asset_name}")"
  else
    die "Neither curl nor wget is available."
  fi
}

verify_updater_asset() {
  local checksum_path="$1"

  grep -F "${UPDATER_ASSET_NAME}" "${checksum_path}" >/dev/null 2>&1 \
    || die "Checksum file does not contain an entry for ${UPDATER_ASSET_NAME}."

  (
    cd "${tmpdir}"
    sha256sum -c --ignore-missing "${checksum_path##*/}"
  ) || die "Checksum verification failed for ${UPDATER_ASSET_NAME}."
}

main() {
  require_command bash
  require_command mktemp
  require_command sha256sum

  if command -v curl >/dev/null 2>&1; then
    : 
  elif command -v wget >/dev/null 2>&1; then
    :
  else
    die "Neither curl nor wget is available."
  fi

  tmpdir="$(mktemp -d)"

  download_asset "${UPDATER_ASSET_NAME}" "${tmpdir}/${UPDATER_ASSET_NAME}"
  download_asset "${CHECKSUMS_ASSET_NAME}" "${tmpdir}/${CHECKSUMS_ASSET_NAME}"
  verify_updater_asset "${tmpdir}/${CHECKSUMS_ASSET_NAME}"
  chmod 755 "${tmpdir}/${UPDATER_ASSET_NAME}"

  bash "${tmpdir}/${UPDATER_ASSET_NAME}" "$@"
}

if [[ -n "${BASH_SOURCE[0]:-}" && "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
