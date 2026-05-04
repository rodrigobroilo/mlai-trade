#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home_dir="${MLAI_TRADE_SMOKE_HOME:-}"
keep_home="${MLAI_TRADE_SMOKE_KEEP_HOME:-0}"

usage() {
  cat <<'USAGE'
Usage: scripts/cli-smoke-test.sh run [MLAI_TRADE_BIN]

Validate fast, no-credential CLI/API/daemon status behavior in a disposable
runtime home. The command never places trades and does not call live providers.

Commands:
  run   Run the smoke test.
  help  Show this help.

Arguments:
  MLAI_TRADE_BIN  Optional path to the binary. Defaults to
                  target/release/mlai-trade or $MLAI_TRADE_BIN.

Environment:
  MLAI_TRADE_BIN              Binary path override
  MLAI_TRADE_SMOKE_HOME       Runtime home to use instead of a temp directory
  MLAI_TRADE_SMOKE_KEEP_HOME=1 Keep the runtime home after the test
USAGE
}

case "${1:-}" in
  -h | --help | help)
    usage
    exit 0
    ;;
  run | test)
    shift || true
    bin="${1:-${MLAI_TRADE_BIN:-${repo_root}/target/release/mlai-trade}}"
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

if [[ ! -x "${bin}" ]]; then
  echo "error: mlai-trade binary is not executable: ${bin}" >&2
  exit 127
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required for JSON smoke validation" >&2
  exit 127
fi

if [[ -z "${home_dir}" ]]; then
  home_dir="$(mktemp -d "${TMPDIR:-/tmp}/mlai-trade-smoke.XXXXXX")"
else
  mkdir -p "${home_dir}"
fi

cleanup() {
  "${bin}" --home "${home_dir}" daemon stop >/dev/null 2>&1 || true
  "${bin}" --home "${home_dir}" api stop >/dev/null 2>&1 || true
  for pid_file in "${home_dir}/tmp/mlai-trade-daemon.pid" "${home_dir}/tmp/mlai-trade-api.pid"; do
    if [[ -f "${pid_file}" ]]; then
      pid="$(tr -d '[:space:]' <"${pid_file}" || true)"
      if [[ "${pid}" =~ ^[0-9]+$ ]]; then
        kill "${pid}" >/dev/null 2>&1 || true
      fi
      rm -f "${pid_file}"
    fi
  done
  if [[ "${keep_home}" != "1" ]]; then
    rm -rf "${home_dir}"
  fi
}
trap cleanup EXIT

prepare_config() {
  mkdir -p "${home_dir}/config"
  jq '
    .daemon.enabled = true
    | .daemon.daily_refresh_enabled = false
    | .api.enabled = true
  ' "${repo_root}/config/mlai-trade.example.json" >"${home_dir}/config/mlai-trade.json"
  cp "${repo_root}/config/tax-brackets.example.json" "${home_dir}/config/tax-brackets.json"
  chmod 600 "${home_dir}/config/mlai-trade.json" "${home_dir}/config/tax-brackets.json"
}

run_json() {
  label="$1"
  shift
  echo "==> ${label}"
  output="$("$@")"
  printf '%s\n' "${output}" | jq empty
}

run_plain() {
  label="$1"
  shift
  echo "==> ${label}"
  "$@" >/dev/null
}

prepare_config

run_json "runtime version JSON" "${bin}" --home "${home_dir}" runtime version --json
run_plain "root help" "${bin}" --home "${home_dir}" --help
for topic in runtime daemon api trade market data compliance feeds ml auto; do
  run_plain "${topic} help" "${bin}" --home "${home_dir}" "${topic}" --help
done

run_plain "api status stopped" "${bin}" --home "${home_dir}" api status
run_plain "api status --details stopped" "${bin}" --home "${home_dir}" api status --details
run_plain "daemon status stopped" "${bin}" --home "${home_dir}" daemon status
run_plain "daemon status --details stopped" "${bin}" --home "${home_dir}" daemon status --details
run_json "data status JSON" "${bin}" --home "${home_dir}" data status --json
run_json "ml status JSON" "${bin}" --home "${home_dir}" ml status --json
run_json "feeds status JSON" "${bin}" --home "${home_dir}" feeds status --json
run_json "auto status JSON" "${bin}" --home "${home_dir}" auto status --json

run_plain "api start" "${bin}" --home "${home_dir}" api start
run_plain "daemon start" "${bin}" --home "${home_dir}" daemon start
sleep 2
run_plain "api status --details running" "${bin}" --home "${home_dir}" api status --details
run_plain "daemon status --details running" "${bin}" --home "${home_dir}" daemon status --details
run_json "api health JSON" "${bin}" --home "${home_dir}" api test --json
run_plain "daemon stop" "${bin}" --home "${home_dir}" daemon stop
run_plain "daemon status --details stopped after stop" "${bin}" --home "${home_dir}" daemon status --details
run_plain "api stop" "${bin}" --home "${home_dir}" api stop
run_plain "api status --details stopped after stop" "${bin}" --home "${home_dir}" api status --details

echo "CLI smoke test passed with home=${home_dir}"
