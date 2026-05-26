#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
docker_bin="${DOCKER_BIN:-docker}"
image="${MLAI_TRADE_LINUX_IMAGE:-mlai-trade:ubuntu-test}"
container_name="${MLAI_TRADE_TEST_CONTAINER:-mlai-trade-ubuntu-test}"
dockerfile="${repo_root}/tests/linux-ubuntu/Dockerfile"
host_os="$(uname -s)"
mode="${1:-}"
platform="${MLAI_TRADE_LINUX_PLATFORM:-}"

if [[ -z "${platform}" && "${host_os}" != "Linux" ]]; then
  platform="linux/amd64"
fi

container_emulation_env=()
if [[ "${host_os}" != "Linux" && "${platform}" == "linux/amd64" ]]; then
  container_emulation_env=(
    -e "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-1}"
    -e "CC=${CC:-clang}"
    -e "CMAKE_BUILD_PARALLEL_LEVEL=${CMAKE_BUILD_PARALLEL_LEVEL:-1}"
    -e "CXX=${CXX:-clang++}"
  )
fi

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
  MLAI_TRADE_LINUX_PLATFORM          Docker platform on non-Linux hosts,
                                     default: linux/amd64
  CARGO_BUILD_JOBS                   Cargo jobs for emulated Docker validation,
                                     default: 1 on macOS linux/amd64
  CC, CXX, CMAKE_BUILD_PARALLEL_LEVEL
                                     C/CMake toolchain knobs for emulated
                                     Docker validation; default clang/clang++,
                                     CMake parallel level 1 on macOS linux/amd64
  MLAI_TRADE_TEST_CONTAINER          Debug container name
  MLAI_TRADE_DOCKER_AUTOINSTALL=0    Disable macOS Docker/Colima auto-install
  MLAI_TRADE_LINUX_IMAGE_UPDATE=1    Force image rebuild
USAGE
}

docker_ready() {
  command -v "${docker_bin}" >/dev/null 2>&1 && "${docker_bin}" info >/dev/null 2>&1
}

ensure_buildx() {
  if [[ -z "${platform}" ]]; then
    return
  fi

  if "${docker_bin}" buildx version >/dev/null 2>&1; then
    return
  fi

  if [[ "${host_os}" == "Darwin" ]]; then
    if ! command -v brew >/dev/null 2>&1; then
      echo "error: Docker buildx is required for ${platform} builds and Homebrew is unavailable" >&2
      exit 127
    fi
    brew list docker-buildx >/dev/null 2>&1 || brew install docker-buildx
    mkdir -p "${HOME}/.docker/cli-plugins"
    if [[ -x /opt/homebrew/lib/docker/cli-plugins/docker-buildx ]]; then
      ln -sf /opt/homebrew/lib/docker/cli-plugins/docker-buildx \
        "${HOME}/.docker/cli-plugins/docker-buildx"
    elif [[ -x /usr/local/lib/docker/cli-plugins/docker-buildx ]]; then
      ln -sf /usr/local/lib/docker/cli-plugins/docker-buildx \
        "${HOME}/.docker/cli-plugins/docker-buildx"
    fi
  fi

  if ! "${docker_bin}" buildx version >/dev/null 2>&1; then
    echo "error: Docker buildx is required for ${platform} image builds" >&2
    exit 127
  fi
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
  cargo check
  cargo test
  cargo build --release
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
  ensure_buildx
  local dockerfile_sha current_arch current_sha current_platform expected_arch platform_matches update
  local build_cmd=("${docker_bin}" build)
  if [[ -n "${platform}" ]]; then
    build_cmd=("${docker_bin}" buildx build --load --platform "${platform}")
  fi
  dockerfile_sha="$(calculate_dockerfile_sha)"
  current_sha="$("${docker_bin}" image inspect \
    --format '{{ index .Config.Labels "mlai-trade.dockerfile-sha" }}' \
    "${image}" 2>/dev/null || true)"
  current_platform="$("${docker_bin}" image inspect \
    --format '{{ index .Config.Labels "mlai-trade.docker-platform" }}' \
    "${image}" 2>/dev/null || true)"
  current_arch="$("${docker_bin}" image inspect \
    --format '{{ .Architecture }}' \
    "${image}" 2>/dev/null || true)"
  expected_arch=""
  case "${platform}" in
    linux/amd64) expected_arch="amd64" ;;
    linux/arm64) expected_arch="arm64" ;;
  esac
  platform_matches="0"
  if [[ -z "${platform}" && "${current_platform}" == "native" ]]; then
    platform_matches="1"
  elif [[ -n "${platform}" && "${current_platform}" == "${platform}" ]]; then
    if [[ -z "${expected_arch}" || "${current_arch}" == "${expected_arch}" ]]; then
      platform_matches="1"
    fi
  fi
  update="${MLAI_TRADE_LINUX_IMAGE_UPDATE:-0}"

  if [[ "${update}" != "1" && "${current_sha}" == "${dockerfile_sha}" && "${platform_matches}" == "1" ]]; then
    echo "Using cached offline Ubuntu test image: ${image} (${platform:-native})"
    return
  fi

  if [[ "${update}" == "1" ]]; then
    echo "Updating Ubuntu test image: ${image} (${platform:-native})"
    "${build_cmd[@]}" \
      --pull \
      --label "mlai-trade.dockerfile-sha=${dockerfile_sha}" \
      --label "mlai-trade.docker-platform=${platform:-native}" \
      -f "${dockerfile}" \
      -t "${image}" \
      "${repo_root}"
  else
    echo "Building Ubuntu test image once for offline reuse: ${image} (${platform:-native})"
    "${build_cmd[@]}" \
      --label "mlai-trade.dockerfile-sha=${dockerfile_sha}" \
      --label "mlai-trade.docker-platform=${platform:-native}" \
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
  local platform_args=()
  if [[ -n "${platform}" ]]; then
    platform_args=(--platform "${platform}")
  fi
  "${docker_bin}" run --rm \
    "${platform_args[@]}" \
    -v "${repo_root}:/workspace:ro" \
    -v mlai-trade-cargo-registry:/root/.cargo/registry \
    -v mlai-trade-cargo-git:/root/.cargo/git \
    -v mlai-trade-target-linux-ubuntu:/tmp/mlai-trade-target \
    -e CARGO_TARGET_DIR=/tmp/mlai-trade-target \
    -e RUSTFLAGS="-D warnings" \
    "${container_emulation_env[@]}" \
    "${image}" \
    bash -lc "${container_copy_command}"'
      cargo fmt --check
      cargo check
      cargo test
      cargo build --release
      scripts/cli-smoke-test.sh run /tmp/mlai-trade-target/release/mlai-trade
      scripts/e2e-synthetic-test.sh run /tmp/mlai-trade-target/release/mlai-trade
      scripts/provider-fake-alpaca-test.sh run /tmp/mlai-trade-target/release/mlai-trade
    '
}

start_debug_container() {
  build_image
  "${docker_bin}" rm -f "${container_name}" >/dev/null 2>&1 || true
  local platform_args=()
  if [[ -n "${platform}" ]]; then
    platform_args=(--platform "${platform}")
  fi
  "${docker_bin}" run -d \
    "${platform_args[@]}" \
    --name "${container_name}" \
    -v "${repo_root}:/workspace:ro" \
    -v mlai-trade-cargo-registry:/root/.cargo/registry \
    -v mlai-trade-cargo-git:/root/.cargo/git \
    -v mlai-trade-target-linux-ubuntu:/tmp/mlai-trade-target \
    -e CARGO_TARGET_DIR=/tmp/mlai-trade-target \
    -e RUSTFLAGS="-D warnings" \
    "${container_emulation_env[@]}" \
    "${image}" \
    bash -lc "${container_copy_command}"' exec sleep infinity'
  echo "Ubuntu test container is running: ${container_name}"
  echo "List containers: docker ps"
  echo "Open shell:      docker exec -it ${container_name} bash"
  echo "Repo copy:       /tmp/mlai-trade-src"
}

open_debug_shell() {
  build_image
  local platform_args=()
  if [[ -n "${platform}" ]]; then
    platform_args=(--platform "${platform}")
  fi
  "${docker_bin}" run --rm -it \
    "${platform_args[@]}" \
    -v "${repo_root}:/workspace:ro" \
    -v mlai-trade-cargo-registry:/root/.cargo/registry \
    -v mlai-trade-cargo-git:/root/.cargo/git \
    -v mlai-trade-target-linux-ubuntu:/tmp/mlai-trade-target \
    -e CARGO_TARGET_DIR=/tmp/mlai-trade-target \
    -e RUSTFLAGS="-D warnings" \
    "${container_emulation_env[@]}" \
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
