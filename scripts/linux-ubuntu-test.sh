#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runtime="${CONTAINER_RUNTIME:-}"
if [[ -z "${runtime}" ]]; then
  if command -v docker >/dev/null 2>&1; then
    runtime="docker"
  elif command -v podman >/dev/null 2>&1; then
    runtime="podman"
  else
    echo "error: docker or podman is required for Ubuntu container tests" >&2
    exit 127
  fi
fi

image="${MLAI_TRADE_LINUX_IMAGE:-mlai-trade:ubuntu-test}"

"${runtime}" build \
  -f "${repo_root}/docker/ubuntu-test/Dockerfile" \
  -t "${image}" \
  "${repo_root}"

"${runtime}" run --rm \
  -v "${repo_root}:/workspace:ro" \
  -v mlai-trade-cargo-registry:/root/.cargo/registry \
  -v mlai-trade-cargo-git:/root/.cargo/git \
  -v mlai-trade-target-linux-ubuntu:/tmp/mlai-trade-target \
  -e CARGO_TARGET_DIR=/tmp/mlai-trade-target \
  "${image}" \
  bash -lc 'set -euo pipefail
    mkdir -p /tmp/mlai-trade-src
    rsync -a --delete --exclude-from=/workspace/.dockerignore /workspace/ /tmp/mlai-trade-src/
    cd /tmp/mlai-trade-src
    cargo fmt --check
    cargo check --no-default-features
    cargo test --no-default-features
    cargo build --release --no-default-features
  '
