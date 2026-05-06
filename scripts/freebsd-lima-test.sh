#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
instance="${MLAI_TRADE_FREEBSD_INSTANCE:-mlai-trade-freebsd16-test}"
template="${MLAI_TRADE_FREEBSD_TEMPLATE:-template:experimental/freebsd-16}"
host_os="$(uname -s)"
mode="${1:-}"

usage() {
  cat <<'USAGE'
Usage: scripts/freebsd-lima-test.sh [COMMAND]

Validate the FreeBSD build path.

Commands:
  run     Run validation.
          On FreeBSD this runs natively. On non-FreeBSD hosts this uses the
          cached Lima FreeBSD 16 VM.
  clean   Remove stale test work directories inside the FreeBSD VM. Preserves
          the cached VM.
  shell   Copy the filtered repo and open a shell inside the FreeBSD guest.
  update  Delete/recreate the cached FreeBSD VM.
  stop    Stop the cached FreeBSD VM.
  delete  Delete the cached FreeBSD VM.
  help    Show this help.

Environment:
  MLAI_TRADE_FREEBSD_INSTANCE         VM name, default: mlai-trade-freebsd16-test
  MLAI_TRADE_FREEBSD_TEMPLATE         Lima template, default: template:experimental/freebsd-16
  MLAI_TRADE_FREEBSD_AUTOINSTALL=0    Disable macOS Lima/QEMU auto-install
  MLAI_TRADE_FREEBSD_CPUS             VM CPU count, default: 4
  MLAI_TRADE_FREEBSD_MEMORY_GB        VM memory, default: 4
  MLAI_TRADE_FREEBSD_DISK_GB          VM disk, default: 30
USAGE
}

qemu_arch() {
  case "$(uname -m)" in
    arm64 | aarch64) echo "aarch64" ;;
    x86_64 | amd64) echo "x86_64" ;;
    *) uname -m ;;
  esac
}

run_rust_validation() {
  export RUSTFLAGS="${RUSTFLAGS:-} -D warnings"
  cargo fmt --check
  cargo check
  cargo test
  cargo build --release
  scripts/cli-smoke-test.sh run target/release/mlai-trade
  scripts/e2e-synthetic-test.sh run target/release/mlai-trade
  scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
}

ensure_lima() {
  local qemu_bin="qemu-system-$(qemu_arch)"
  if command -v limactl >/dev/null 2>&1 && command -v "${qemu_bin}" >/dev/null 2>&1; then
    return
  fi
  if [[ "${host_os}" == "Darwin" && "${MLAI_TRADE_FREEBSD_AUTOINSTALL:-1}" != "0" ]]; then
    if ! command -v brew >/dev/null 2>&1; then
      echo "error: Homebrew is required for automatic Lima setup on macOS" >&2
      exit 127
    fi
    if ! command -v limactl >/dev/null 2>&1 || ! command -v "${qemu_bin}" >/dev/null 2>&1; then
      brew install lima qemu
    fi
    return
  fi
  echo "error: limactl is required for FreeBSD VM validation on ${host_os}" >&2
  exit 127
}

instance_status() {
  limactl list --format '{{.Name}} {{.Status}}' 2>/dev/null \
    | awk -v name="${instance}" '$1 == name {print $2; found=1} END {if (!found) print ""}'
}

ensure_instance() {
  ensure_lima
  local status
  status="$(instance_status)"
  if [[ -z "${status}" ]]; then
    echo "Creating cached FreeBSD validation VM: ${instance} (${template})"
    limactl start -y \
      --name="${instance}" \
      --cpus "${MLAI_TRADE_FREEBSD_CPUS:-4}" \
      --memory "${MLAI_TRADE_FREEBSD_MEMORY_GB:-4}" \
      --disk "${MLAI_TRADE_FREEBSD_DISK_GB:-30}" \
      --mount-none \
      "${template}"
    return
  fi
  if [[ "${status}" != "Running" ]]; then
    echo "Starting cached FreeBSD validation VM: ${instance}"
    limactl start -y "${instance}"
  else
    echo "Using running cached FreeBSD validation VM: ${instance}"
  fi
}

guest_sh() {
  limactl shell -y "${instance}" "$@"
}

copy_repo_to_guest() {
  local copy_root
  copy_root="$(mktemp -d "${TMPDIR:-/tmp}/mlai-trade-freebsd-copy.XXXXXX")"
  rsync -a --delete --exclude-from="${repo_root}/.dockerignore" "${repo_root}/" "${copy_root}/"
  guest_sh sh -lc 'rm -rf /tmp/mlai-trade-src && mkdir -p /tmp/mlai-trade-src'
  limactl copy -y --backend=scp -r "${copy_root}/." "${instance}:/tmp/mlai-trade-src/"
  rm -rf "${copy_root}"
}

clean_guest_test_state() {
  guest_sh sh -lc '
    rm -rf /tmp/mlai-trade-src /tmp/mlai-trade-smoke.* /tmp/mlai-trade-e2e.*
  ' >/dev/null 2>&1 || true
}

install_guest_deps() {
  guest_sh sh -lc '
    set -e
    if ! command -v pkg >/dev/null 2>&1; then
      echo "error: pkg is not available in the FreeBSD guest" >&2
      exit 127
    fi
    sudo env ASSUME_ALWAYS_YES=yes pkg bootstrap -f >/dev/null 2>&1 || true
    sudo pkg install -y \
      bash \
      ca_root_nss \
      cmake \
      curl \
      git \
      jq \
      llvm \
      openssl \
      pkgconf \
      rust \
      sqlite3
  '
}

run_guest_validation() {
  ensure_instance
  clean_guest_test_state
  install_guest_deps
  copy_repo_to_guest
  guest_sh bash -lc '
    set -euo pipefail
    cd /tmp/mlai-trade-src
    export RUSTFLAGS="${RUSTFLAGS:-} -D warnings"
    cargo fmt --check
    cargo check
    cargo test
    cargo build --release
    scripts/cli-smoke-test.sh run target/release/mlai-trade
    scripts/e2e-synthetic-test.sh run target/release/mlai-trade
    scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
  '
}

open_guest_shell() {
  ensure_instance
  copy_repo_to_guest
  limactl shell -y --workdir /tmp/mlai-trade-src "${instance}" bash
}

case "${mode}" in
  run | test)
    if [[ "${host_os}" == "FreeBSD" ]]; then
      run_rust_validation
    else
      run_guest_validation
    fi
    ;;
  shell)
    open_guest_shell
    ;;
  update)
    ensure_lima
    limactl stop -y "${instance}" >/dev/null 2>&1 || true
    limactl delete -y "${instance}" >/dev/null 2>&1 || true
    ensure_instance
    ;;
  clean)
    ensure_instance
    clean_guest_test_state
    echo "Removed stale FreeBSD guest test directories in ${instance}"
    ;;
  stop)
    ensure_lima
    limactl stop -y "${instance}"
    ;;
  delete)
    ensure_lima
    limactl stop -y "${instance}" >/dev/null 2>&1 || true
    limactl delete -y "${instance}"
    ;;
  -h | --help | help)
    usage
    ;;
  "")
    usage >&2
    exit 2
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
