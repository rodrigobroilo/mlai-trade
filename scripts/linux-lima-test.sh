#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
instance="${MLAI_TRADE_LINUX_INSTANCE:-mlai-trade-linux-amd64-test}"
template="${MLAI_TRADE_LINUX_TEMPLATE:-template:ubuntu-24.04}"
arch="${MLAI_TRADE_LINUX_ARCH:-x86_64}"
vm_type="${MLAI_TRADE_LINUX_VM_TYPE:-qemu}"
host_os="$(uname -s)"
mode="${1:-}"

usage() {
  cat <<'USAGE'
Usage: scripts/linux-lima-test.sh [COMMAND]

Validate the Linux build path.

Commands:
  run     Run validation.
          On Linux this runs natively. On non-Linux hosts this uses the cached
          Lima Ubuntu 24.04 x86_64 VM.
  clean   Remove stale test work directories inside the Linux VM. Preserves the
          cached VM.
  shell   Copy the filtered repo and open a shell inside the Linux guest.
  update  Delete/recreate the cached Linux VM.
  stop    Stop the cached Linux VM.
  delete  Delete the cached Linux VM.
  help    Show this help.

Environment:
  MLAI_TRADE_LINUX_INSTANCE          VM name, default: mlai-trade-linux-amd64-test
  MLAI_TRADE_LINUX_TEMPLATE          Lima template, default: template:ubuntu-24.04
  MLAI_TRADE_LINUX_ARCH              VM arch, default: x86_64
  MLAI_TRADE_LINUX_VM_TYPE           Lima VM type, default: qemu
  MLAI_TRADE_LINUX_AUTOINSTALL=0     Disable macOS Lima/QEMU auto-install
  MLAI_TRADE_LINUX_CPUS              VM CPU count, default: 4
  MLAI_TRADE_LINUX_MEMORY_GB         VM memory, default: 12
  MLAI_TRADE_LINUX_DISK_GB           VM disk, default: 80
  MLAI_TRADE_LINUX_CARGO_JOBS        Guest Cargo jobs, default: 2
  MLAI_TRADE_LINUX_CMAKE_JOBS        Guest CMake jobs, default: 2
USAGE
}

qemu_binary_for_arch() {
  case "${arch}" in
    aarch64 | arm64) echo "qemu-system-aarch64" ;;
    x86_64 | amd64) echo "qemu-system-x86_64" ;;
    *) echo "qemu-system-${arch}" ;;
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
  local qemu_bin
  qemu_bin="$(qemu_binary_for_arch)"
  if command -v limactl >/dev/null 2>&1 && command -v "${qemu_bin}" >/dev/null 2>&1; then
    return
  fi

  if [[ "${host_os}" == "Darwin" && "${MLAI_TRADE_LINUX_AUTOINSTALL:-1}" != "0" ]]; then
    if ! command -v brew >/dev/null 2>&1; then
      echo "error: Homebrew is required for automatic Lima setup on macOS" >&2
      exit 127
    fi
    if ! command -v limactl >/dev/null 2>&1 || ! command -v "${qemu_bin}" >/dev/null 2>&1; then
      brew install lima qemu
    fi
    return
  fi

  echo "error: limactl and ${qemu_bin} are required for Linux VM validation on ${host_os}" >&2
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
    echo "Creating cached Linux validation VM: ${instance} (${template}, ${arch})"
    limactl start -y \
      --name="${instance}" \
      --arch "${arch}" \
      --vm-type "${vm_type}" \
      --cpus "${MLAI_TRADE_LINUX_CPUS:-4}" \
      --memory "${MLAI_TRADE_LINUX_MEMORY_GB:-12}" \
      --disk "${MLAI_TRADE_LINUX_DISK_GB:-80}" \
      --mount-none \
      "${template}"
    return
  fi

  if [[ "${status}" != "Running" ]]; then
    echo "Starting cached Linux validation VM: ${instance}"
    limactl start -y "${instance}"
  else
    echo "Using running cached Linux validation VM: ${instance}"
  fi
}

guest_sh() {
  limactl shell -y "${instance}" "$@"
}

copy_repo_to_guest() {
  local copy_root exclude_file
  copy_root="$(mktemp -d "${TMPDIR:-/tmp}/mlai-trade-linux-copy.XXXXXX")"
  exclude_file="${repo_root}/tests/repo-sync.exclude"
  if [[ -f "${exclude_file}" ]]; then
    rsync -a --delete --exclude-from="${exclude_file}" "${repo_root}/" "${copy_root}/"
  else
    rsync -a --delete "${repo_root}/" "${copy_root}/"
  fi
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
  guest_sh bash -lc '
    set -euo pipefail
    export DEBIAN_FRONTEND=noninteractive
    sudo apt-get update
    sudo apt-get install -y --no-install-recommends \
      build-essential \
      ca-certificates \
      clang \
      cmake \
      curl \
      git \
      jq \
      libclang-dev \
      libomp-dev \
      libssl-dev \
      pkg-config \
      rsync \
      sqlite3 \
      zlib1g-dev
    if ! command -v cargo >/dev/null 2>&1; then
      curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
    fi
    export PATH="${HOME}/.cargo/bin:${PATH}"
    rustup component add rustfmt
  '
}

run_guest_validation() {
  ensure_instance
  clean_guest_test_state
  install_guest_deps
  copy_repo_to_guest
  guest_sh bash -lc '
    set -euo pipefail
    export PATH="${HOME}/.cargo/bin:${PATH}"
    export RUSTFLAGS="${RUSTFLAGS:-} -D warnings"
    export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-'"${MLAI_TRADE_LINUX_CARGO_JOBS:-2}"'}"
    export CMAKE_BUILD_PARALLEL_LEVEL="${CMAKE_BUILD_PARALLEL_LEVEL:-'"${MLAI_TRADE_LINUX_CMAKE_JOBS:-2}"'}"
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/mlai-trade-target}"
    cd /tmp/mlai-trade-src
    cargo fmt --check
    cargo check
    cargo test
    cargo build --release
    scripts/cli-smoke-test.sh run "${CARGO_TARGET_DIR}/release/mlai-trade"
    scripts/e2e-synthetic-test.sh run "${CARGO_TARGET_DIR}/release/mlai-trade"
    scripts/provider-fake-alpaca-test.sh run "${CARGO_TARGET_DIR}/release/mlai-trade"
  '
}

open_guest_shell() {
  ensure_instance
  copy_repo_to_guest
  limactl shell -y --workdir /tmp/mlai-trade-src "${instance}" bash
}

case "${mode}" in
  run | test)
    if [[ "${host_os}" == "Linux" ]]; then
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
    echo "Removed stale Linux guest test directories in ${instance}"
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
