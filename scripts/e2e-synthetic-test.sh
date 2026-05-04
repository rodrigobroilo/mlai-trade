#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home_dir="${MLAI_TRADE_E2E_HOME:-}"
keep_home="${MLAI_TRADE_E2E_KEEP_HOME:-0}"
with_lstm="${MLAI_TRADE_E2E_WITH_LSTM:-1}"

usage() {
  cat <<'USAGE'
Usage: scripts/e2e-synthetic-test.sh run [MLAI_TRADE_BIN]

Validate the non-trading data, feeds, ML, API, and daemon path with fake
stock/ETF data. The command never places trades and does not call live
providers.

Commands:
  run   Run the synthetic end-to-end test.
  help  Show this help.

Arguments:
  MLAI_TRADE_BIN  Optional path to the binary. Defaults to
                  target/release/mlai-trade or $MLAI_TRADE_BIN.

Environment:
  MLAI_TRADE_BIN              Binary path override
  MLAI_TRADE_E2E_HOME         Runtime home to use instead of a temp directory
  MLAI_TRADE_E2E_KEEP_HOME=1  Keep the runtime home after the test
  MLAI_TRADE_E2E_WITH_LSTM=0  Skip CPU LSTM training/prediction
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
  echo "error: jq is required for JSON validation" >&2
  exit 127
fi

if [[ -z "${home_dir}" ]]; then
  home_dir="$(mktemp -d "${TMPDIR:-/tmp}/mlai-trade-e2e.XXXXXX")"
else
  mkdir -p "${home_dir}"
fi

cleanup() {
  "${bin}" --home "${home_dir}" daemon stop >/dev/null 2>&1 || true
  "${bin}" --home "${home_dir}" api stop >/dev/null 2>&1 || true
  if [[ "${keep_home}" != "1" ]]; then
    rm -rf "${home_dir}"
  fi
}
trap cleanup EXIT

run_plain() {
  label="$1"
  shift
  echo "==> ${label}"
  "$@"
}

run_json() {
  label="$1"
  shift
  echo "==> ${label}"
  output="$("$@")"
  printf '%s\n' "${output}" | jq empty
}

export MLAI_TRADE_BIN="${bin}"
"${repo_root}/scripts/seed-synthetic-market.sh" run "${home_dir}"

run_json "data status after synthetic seed" "${bin}" --home "${home_dir}" data status --json
run_json "feeds status after synthetic seed" "${bin}" --home "${home_dir}" feeds status --json
run_json "feeds sentiment AAPL" "${bin}" --home "${home_dir}" feeds sentiment AAPL --json
run_plain "data screen synthetic" "${bin}" --home "${home_dir}" data screen --min-volume 100000
run_json "data suggest synthetic JSON" "${bin}" --home "${home_dir}" data suggest --json

run_plain "ml features synthetic" "${bin}" --home "${home_dir}" ml features --force
run_plain "ml labels synthetic" "${bin}" --home "${home_dir}" ml labels --horizon 5
run_plain "ml train quick synthetic" "${bin}" --home "${home_dir}" ml train --quick
run_plain "ml predict synthetic" "${bin}" --home "${home_dir}" ml predict
run_plain "ml baselines quick synthetic" "${bin}" --home "${home_dir}" ml baselines --quick
run_plain "ml walk-forward quick synthetic" "${bin}" --home "${home_dir}" ml walk-forward --quick --folds 2

if [[ "${with_lstm}" == "1" ]]; then
  run_plain "ml lstm-train cpu synthetic" "${bin}" --home "${home_dir}" ml lstm-train --backend cpu --single-thread
  run_plain "ml lstm-predict synthetic" "${bin}" --home "${home_dir}" ml lstm-predict
fi

run_plain "ml ensemble synthetic" "${bin}" --home "${home_dir}" ml ensemble
run_json "ml status final synthetic JSON" "${bin}" --home "${home_dir}" ml status --json
run_plain "ml explainable synthetic" "${bin}" --home "${home_dir}" ml explainable
run_plain "api start synthetic" "${bin}" --home "${home_dir}" api start
run_plain "daemon start synthetic" "${bin}" --home "${home_dir}" daemon start
sleep 2
run_plain "api status details synthetic" "${bin}" --home "${home_dir}" api status --details
run_plain "daemon status details synthetic" "${bin}" --home "${home_dir}" daemon status --details
run_json "api health synthetic JSON" "${bin}" --home "${home_dir}" api test --json
run_plain "daemon stop synthetic" "${bin}" --home "${home_dir}" daemon stop
run_plain "api stop synthetic" "${bin}" --home "${home_dir}" api stop

echo "Synthetic end-to-end test passed with home=${home_dir}"
