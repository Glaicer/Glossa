#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
fixture="$(mktemp -d)"
trap 'rm -rf "${fixture}"' EXIT
export TEST_REPO="${repo}" TEST_FIXTURE="${fixture}"
export HOME="${fixture}/home"
mkdir -p "${HOME}/.local/bin" "${HOME}/.local/share/glossa/assets/tray" \
  "${HOME}/.local/share/glossa/assets/sounds" "${HOME}/.config/glossa" "${fixture}/bin"
export TEST_SERVICE_LOG="${fixture}/services.log"
cat > "${fixture}/bin/systemctl" <<'SH'
#!/usr/bin/env bash
set -eu
printf '%s\n' "$*" >> "${TEST_SERVICE_LOG}"
if [[ "$*" == '--user disable --now glossa.service' ]]; then
  test -x "${HOME}/.local/bin/glossa"
  printf 'binary-present-at-stop\n' >> "${TEST_SERVICE_LOG}"
fi
SH
cat > "${fixture}/bin/deny" <<'SH'
#!/usr/bin/env bash
printf 'Forbidden fixture command: %s\n' "$0" >&2
exit 99
SH
chmod +x "${fixture}/bin/systemctl" "${fixture}/bin/deny"
for command in sudo curl wget apt-get dnf yum udevadm; do
  ln -s deny "${fixture}/bin/${command}"
done
export PATH="${fixture}/bin:${PATH}"

uninstall_preserves_user_files() {
  printf '#!/bin/sh\nexit 0\n' > "${HOME}/.local/bin/glossa"
  chmod +x "${HOME}/.local/bin/glossa"
  cp "${repo}/update.sh" "${HOME}/.local/bin/update.sh"
  printf 'test-version\n' > "${HOME}/.local/share/glossa/VERSION"
  for name in idle recording processing idle_dark recording_dark processing_dark; do
    touch "${HOME}/.local/share/glossa/assets/tray/${name}.png"
  done
  touch "${HOME}/.local/share/glossa/assets/sounds/start.wav" \
    "${HOME}/.local/share/glossa/assets/sounds/stop.wav"
  printf 'custom tray\n' > "${HOME}/.local/share/glossa/assets/tray/user.png"
  printf 'custom sound\n' > "${HOME}/.local/share/glossa/assets/sounds/user.wav"
  printf '[ui]\noverlay = false\n# custom config\n' > "${HOME}/.config/glossa/config.toml"
  cp "${HOME}/.config/glossa/config.toml" "${fixture}/config.before"
  bash -c 'source "${TEST_REPO}/uninstall.sh"; remove_glossa_installation'
  grep -Fxq 'binary-present-at-stop' "${TEST_SERVICE_LOG}"
  test ! -e "${HOME}/.local/bin/glossa"
  test ! -e "${HOME}/.local/bin/update.sh"
  test ! -e "${HOME}/.local/share/glossa/VERSION"
  test ! -e "${HOME}/.local/share/glossa/assets/tray/idle.png"
  test ! -e "${HOME}/.local/share/glossa/assets/sounds/start.wav"
  test -f "${HOME}/.local/share/glossa/assets/tray/user.png"
  test -f "${HOME}/.local/share/glossa/assets/sounds/user.wav"
  cmp "${fixture}/config.before" "${HOME}/.config/glossa/config.toml"
  printf '#!/bin/sh\nexit 0\n' > "${HOME}/.local/bin/glossa"
  chmod +x "${HOME}/.local/bin/glossa"
  printf 'user updater\n' > "${HOME}/.local/bin/update.sh"
  bash -c 'source "${TEST_REPO}/uninstall.sh"; remove_glossa_installation'
  test "$(cat "${HOME}/.local/bin/update.sh")" = 'user updater'
  printf 'PASS uninstall ownership and preservation\n'
}

prepare_bundle() {
  mkdir -p "${fixture}/source/target/release" "${fixture}/source/contrib"
  cp "${repo}/build-release-tarball.sh" "${repo}/update.sh" "${fixture}/source/"
  cp -r "${repo}/contrib/assets" "${repo}/contrib/release" "${fixture}/source/contrib/"
  mkdir -p "${fixture}/source/contrib/dotool/binary"
  for name in dotool dotoolc dotoold 80-dotool.rules; do
    printf 'fixture payload\n' > "${fixture}/source/contrib/dotool/binary/${name}"
  done
  printf '#!/bin/sh\nexit 0\n' > "${fixture}/source/target/release/glossa"
  bash -c '
    source "${TEST_FIXTURE}/source/build-release-tarball.sh"
    release_version="fixture-03"
    set_target_dir
    mkdir -p "${target_dir}"
    staging_dir="$(mktemp -d)"
    stage_release_contents
    render_release_updater
    write_archive
    write_checksums
  '
  export TEST_RELEASE="${fixture}/source/target/release/github/fixture-03"
}

update_preserves_user_files() {
  prepare_bundle
  printf '#!/bin/sh\nexit 0\n' > "${HOME}/.local/bin/glossa"
  chmod +x "${HOME}/.local/bin/glossa"
  printf 'older\n' > "${HOME}/.local/share/glossa/VERSION"
  printf '[ui]\noverlay = false\n# custom config\n' > "${HOME}/.config/glossa/config.toml"
  cp "${HOME}/.config/glossa/config.toml" "${fixture}/config.before"
  printf 'private environment\n' > "${HOME}/.config/glossa/glossa.env"
  cp "${HOME}/.config/glossa/glossa.env" "${fixture}/env.before"
  printf 'custom tray\n' > "${HOME}/.local/share/glossa/assets/tray/user.png"
  printf 'custom sound\n' > "${HOME}/.local/share/glossa/assets/sounds/user.wav"
  bash -c '
    source "${TEST_RELEASE}/glossa-update.sh"
    download_asset() { cp "${TEST_RELEASE}/$1" "$2"; }
    main install
  ' > "${fixture}/update.log"
  grep -Fxq 'GLOSSA_UPDATE_STATUS=updated' "${fixture}/update.log"
  grep -Fxq 'GLOSSA_UPDATE_VERSION=fixture-03' "${fixture}/update.log"
  grep -Fxq -- '--user restart glossa.service' "${TEST_SERVICE_LOG}"
  cmp "${fixture}/config.before" "${HOME}/.config/glossa/config.toml"
  cmp "${fixture}/env.before" "${HOME}/.config/glossa/glossa.env"
  test -f "${HOME}/.local/share/glossa/assets/tray/user.png"
  test -f "${HOME}/.local/share/glossa/assets/sounds/user.wav"
  test -x "${HOME}/.local/bin/glossa"
  bash -c '
    source "${TEST_REPO}/update.sh"
    download_asset() { cp "${TEST_RELEASE}/$1" "$2"; }
    main check
  ' > "${fixture}/bootstrap.log"
  grep -Fxq 'GLOSSA_UPDATE_STATUS=up-to-date' "${fixture}/bootstrap.log"
  printf 'older\n' > "${HOME}/.local/share/glossa/VERSION"
  cp "${HOME}/.local/bin/glossa" "${fixture}/binary.before"
  cp "${TEST_SERVICE_LOG}" "${fixture}/services.before"
  printf 'corrupt archive\n' > "${TEST_RELEASE}/glossa-linux-x86_64.tar.gz"
  if bash -c '
    source "${TEST_RELEASE}/glossa-update.sh"
    download_asset() { cp "${TEST_RELEASE}/$1" "$2"; }
    main install
  ' > "${fixture}/checksum.log" 2>&1; then
    printf 'Corrupt archive unexpectedly installed\n' >&2
    exit 1
  fi
  grep -Fq 'Checksum verification failed' "${fixture}/checksum.log"
  cmp "${fixture}/binary.before" "${HOME}/.local/bin/glossa"
  cmp "${fixture}/services.before" "${TEST_SERVICE_LOG}"
  printf 'PASS update, bootstrap, checksums and sentinel preservation\n'
}

bundle_and_install() {
  prepare_bundle
  (cd "${TEST_RELEASE}" && sha256sum -c sha256sums.txt)
  tar -tzf "${TEST_RELEASE}/glossa-linux-x86_64.tar.gz" | LC_ALL=C sort > "${fixture}/members"
  cat > "${fixture}/expected" <<'MEMBERS'
VERSION
assets/
assets/sounds/
assets/sounds/start.wav
assets/sounds/stop.wav
assets/tray/
assets/tray/idle.png
assets/tray/idle_dark.png
assets/tray/processing.png
assets/tray/processing_dark.png
assets/tray/recording.png
assets/tray/recording_dark.png
dotool/
dotool/80-dotool.rules
dotool/dotool
dotool/dotoolc
dotool/dotoold
glossa
update.sh
MEMBERS
  cmp "${fixture}/expected" "${fixture}/members"
  bash -c '
    source "${TEST_REPO}/install.sh"
    tmpdir="$(mktemp -d)"
    extract_glossa_bundle "${TEST_RELEASE}/glossa-linux-x86_64.tar.gz"
    install_glossa_bundle "${glossa_bundle_dir}"
    wl_copy_path="wl-copy"
    dotoolc_path="dotoolc"
    export TEST_PROVIDER_KEY="fixture-only"
    provider_api_key="env:TEST_PROVIDER_KEY"
    provider_model="custom-model"
    write_config_file
    cp "${CONFIG_PATH}" "${TEST_FIXTURE}/fresh.config"
    read_tty() { printf "y\n"; }
    maybe_generate_config 3>/dev/null
    cmp "${TEST_FIXTURE}/fresh.config" "${CONFIG_PATH}"
    printf "\n# keep this user comment\n" >> "${CONFIG_PATH}"
    sed -i "s/^sample_rate_hz = 16000/sample_rate_hz = 48000/; s/^append_space = false/append_space = true/" "${CONFIG_PATH}"
    sed -i "/^\[ui\]/a overlay = false" "${CONFIG_PATH}"
    cp "${CONFIG_PATH}" "${TEST_FIXTURE}/custom.config"
    maybe_generate_config 3>/dev/null
    cmp "${TEST_FIXTURE}/custom.config" "${CONFIG_PATH}"
    configure_new_install() { :; }
    read_tty() { printf "n\n"; }
    maybe_generate_config 3>/dev/null
    cmp "${TEST_FIXTURE}/custom.config" "${CONFIG_PATH}."*.bak
  '
  test -x "${HOME}/.local/bin/glossa"
  test -x "${HOME}/.local/bin/update.sh"
  test "$(cat "${HOME}/.local/share/glossa/VERSION")" = fixture-03
  cmp "${fixture}/source/target/release/glossa" "${HOME}/.local/bin/glossa"
  cmp "${repo}/update.sh" "${HOME}/.local/bin/update.sh"
  test "$(stat -c %a "${HOME}/.local/bin/glossa")" = 755
  test "$(stat -c %a "${HOME}/.local/share/glossa/assets/tray/idle.png")" = 644
  printf 'PASS bundle layout, installation, config defaults/keep/backup\n'
}

case "${1:-all}" in
  uninstall) uninstall_preserves_user_files ;;
  update) update_preserves_user_files ;;
  bundle) bundle_and_install ;;
  all)
    bash "$0" uninstall
    bash "$0" update
    bash "$0" bundle
    ;;
  *) exit 2 ;;
esac
