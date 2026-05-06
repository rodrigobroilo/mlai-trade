#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
docker_bin="${DOCKER_BIN:-docker}"
image="${MLAI_TRADE_LINUX_IMAGE:-mlai-trade:ubuntu-test}"
container_name="${MLAI_TRADE_TEST_CONTAINER:-mlai-trade-ubuntu-test}"
dockerfile="${repo_root}/tests/linux-ubuntu/Dockerfile"
host_os="$(uname -s)"
mode="${1:-}"

usage() {
  cat <<'USAGE'
Usage: scripts/linux-ubuntu-test.sh [COMMAND]

Validate the Linux build path.

Commands:
  run        Run validation.
             On Linux this runs natively. On non-Linux hosts this uses the
             cached Ubuntu Docker image.
  clean      Remove stale kept test containers. Preserves cached image/volumes.
  container  Start and keep a named Ubuntu container for inspection.
  shell      Open a temporary interactive Ubuntu shell.
  update     Pull/rebuild the cached Ubuntu Docker image intentionally.
  delete     Remove the kept container, cached image, and build-cache volumes.
  help       Show this help.

Environment:
  DOCKER_BIN                         Docker CLI path, default: docker
  MLAI_TRADE_LINUX_IMAGE             Image name, default: mlai-trade:ubuntu-test
  MLAI_TRADE_TEST_CONTAINER          Debug container name
  MLAI_TRADE_DOCKER_AUTOINSTALL=0    Disable macOS Docker/Colima auto-install
  MLAI_TRADE_LINUX_IMAGE_UPDATE=1    Force image rebuild
USAGE
}

docker_ready() {
  command -v "${docker_bin}" >/dev/null 2>&1 && "${docker_bin}" info >/dev/null 2>&1
}

calculate_dockerfile_sha() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${dockerfile}" | awk '{print $1}'
  else
    sha256sum "${dockerfile}" | awk '{print $1}'
  fi
}

run_rust_validation() {
  export RUSTFLAGS="${RUSTFLAGS:-} -D warnings"
  cargo fmt --check
  cargo check --all-features
  cargo test --all-features
  cargo build --release --all-features
  scripts/cli-smoke-test.sh run target/release/mlai-trade
  scripts/e2e-synthetic-test.sh run target/release/mlai-trade
  scripts/provider-fake-alpaca-test.sh run target/release/mlai-trade
}

clean_debug_container() {
  if docker_ready; then
    "${docker_bin}" rm -f "${container_name}" >/dev/null 2>&1 || true
  fi
}

ensure_docker() {
  if docker_ready; then
    return
  fi

  if [[ "${host_os}" == "Linux" ]]; then
    echo "error: Docker is required only for container/shell mode on Linux" >&2
    echo "       For normal Linux validation, run: $0" >&2
    exit 127
  fi

  if [[ "${host_os}" != "Darwin" ]]; then
    echo "error: Docker is required on ${host_os}; automatic setup is only supported on macOS" >&2
    exit 127
  fi

  if [[ "${MLAI_TRADE_DOCKER_AUTOINSTALL:-1}" == "0" ]]; then
    echo "error: Docker is not ready and automatic macOS setup is disabled" >&2
    exit 127
  fi

  if ! command -v brew >/dev/null 2>&1; then
    echo "error: Homebrew is required for automatic Docker CLI/Colima setup on macOS" >&2
    exit 127
  fi

  if ! command -v "${docker_bin}" >/dev/null 2>&1 || ! command -v colima >/dev/null 2>&1; then
    brew install docker colima
  fi

  if ! colima status >/dev/null 2>&1; then
    colima start \
      --cpu "${COLIMA_CPUS:-4}" \
      --memory "${COLIMA_MEMORY_GB:-4}" \
      --disk "${COLIMA_DISK_GB:-30}" \
      --runtime docker
  fi

  if ! docker_ready && [[ -S "${HOME}/.colima/default/docker.sock" ]]; then
    export DOCKER_HOST="unix://${HOME}/.colima/default/docker.sock"
  fi

  if ! docker_ready; then
    echo "error: Docker CLI is installed, but Docker engine is not reachable" >&2
    exit 125
  fi
}

build_image() {
  ensure_docker
  local dockerfile_sha current_sha update
  dockerfile_sha="$(calculate_dockerfile_sha)"
  current_sha="$("${docker_bin}" image inspect \
    --format '{{ index .Config.Labels "mlai-trade.dockerfile-sha" }}' \
    "${image}" 2>/dev/null || true)"
  update="${MLAI_TRADE_LINUX_IMAGE_UPDATE:-0}"

  if [[ "${update}" != "1" && "${current_sha}" == "${dockerfile_sha}" ]]; then
    echo "Using cached offline Ubuntu test image: ${image}"
    return
  fi

  if [[ "${update}" == "1" ]]; then
    echo "Updating Ubuntu test image: ${image}"
    "${docker_bin}" build \
      --pull \
      --label "mlai-trade.dockerfile-sha=${dockerfile_sha}" \
      -f "${dockerfile}" \
      -t "${image}" \
      "${repo_root}"
  else
    echo "Building Ubuntu test image once for offline reuse: ${image}"
    "${docker_bin}" build \
      --label "mlai-trade.dockerfile-sha=${dockerfile_sha}" \
      -f "${dockerfile}" \
      -t "${image}" \
      "${repo_root}"
  fi
}

container_copy_command='set -euo pipefail
  mkdir -p /tmp/mlai-trade-src
  rsync -a --delete --exclude-from=/workspace/.dockerignore /workspace/ /tmp/mlai-trade-src/
  cd /tmp/mlai-trade-src
'

run_container_validation() {
  build_image
  clean_debug_container
  "${docker_bin}" run --rm \
    -v "${repo_root}:/workspace:ro" \
    -v mlai-trade-cargo-registry:/root/.cargo/registry \
    -v mlai-trade-cargo-git:/root/.cargo/git \
    -v mlai-trade-target-linux-ubuntu:/tmp/mlai-trade-target \
    -e CARGO_TARGET_DIR=/tmp/mlai-trade-target \
    -e RUSTFLAGS="-D warnings" \
    "${image}" \
    bash -lc "${container_copy_command}"'
      cargo fmt --check
      cargo check --all-features
      cargo test --all-features
      cargo build --release --all-features
      scripts/cli-smoke-test.sh run /tmp/mlai-trade-target/release/mlai-trade
      scripts/e2e-synthetic-test.sh run /tmp/mlai-trade-target/release/mlai-trade
      scripts/provider-fake-alpaca-test.sh run /tmp/mlai-trade-target/release/mlai-trade
    '
}

start_debug_container() {
  build_image
  "${docker_bin}" rm -f "${container_name}" >/dev/null 2>&1 || true
  "${docker_bin}" run -d \
    --name "${container_name}" \
    -v "${repo_root}:/workspace:ro" \
    -v mlai-trade-cargo-registry:/root/.cargo/registry \
    -v mlai-trade-cargo-git:/root/.cargo/git \
    -v mlai-trade-target-linux-ubuntu:/tmp/mlai-trade-target \
    -e CARGO_TARGET_DIR=/tmp/mlai-trade-target \
    -e RUSTFLAGS="-D warnings" \
    "${image}" \
    bash -lc "${container_copy_command}"' exec sleep infinity'
  echo "Ubuntu test container is running: ${container_name}"
  echo "List containers: docker ps"
  echo "Open shell:      docker exec -it ${container_name} bash"
  echo "Repo copy:       /tmp/mlai-trade-src"
}

open_debug_shell() {
  build_image
  "${docker_bin}" run --rm -it \
    -v "${repo_root}:/workspace:ro" \
    -v mlai-trade-cargo-registry:/root/.cargo/registry \
    -v mlai-trade-cargo-git:/root/.cargo/git \
    -v mlai-trade-target-linux-ubuntu:/tmp/mlai-trade-target \
    -e CARGO_TARGET_DIR=/tmp/mlai-trade-target \
    -e RUSTFLAGS="-D warnings" \
    "${image}" \
    bash -lc "${container_copy_command}"' exec bash'
}

case "${mode}" in
  run | test)
    if [[ "${host_os}" == "Linux" ]]; then
      run_rust_validation
    else
      run_container_validation
    fi
    ;;
  container | keep)
    start_debug_container
    ;;
  shell)
    open_debug_shell
    ;;
  update)
    ensure_docker
    MLAI_TRADE_LINUX_IMAGE_UPDATE=1 build_image
    ;;
  clean)
    ensure_docker
    clean_debug_container
    echo "Removed stale Ubuntu test container, if present: ${container_name}"
    ;;
  delete)
    ensure_docker
    clean_debug_container
    "${docker_bin}" image rm -f "${image}" >/dev/null 2>&1 || true
    "${docker_bin}" volume rm -f \
      mlai-trade-cargo-registry \
      mlai-trade-cargo-git \
      mlai-trade-target-linux-ubuntu >/dev/null 2>&1 || true
    echo "Removed Ubuntu test image/container/cache volumes for ${image}"
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
