#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home_dir="${MLAI_TRADE_FAKE_ALPACA_HOME:-}"
keep_home="${MLAI_TRADE_FAKE_ALPACA_KEEP_HOME:-0}"

usage() {
  cat <<'USAGE'
Usage: scripts/provider-fake-alpaca-test.sh run [MLAI_TRADE_BIN]

Validate Alpaca-provider CLI and Unix-socket API paths against the local fake
Alpaca HTTP fixture. The fixture serves one month of deterministic stock/ETF
market data plus paper account/order/position endpoints. No live provider is
called and no real order can be placed.

Commands:
  run   Run the fake Alpaca provider end-to-end test.
  help  Show this help.

Arguments:
  MLAI_TRADE_BIN  Optional path to the binary. Defaults to
                  target/release/mlai-trade or $MLAI_TRADE_BIN.

Environment:
  MLAI_TRADE_BIN                    Binary path override
  MLAI_TRADE_FAKE_ALPACA_HOME       Runtime home to use instead of a temp dir
  MLAI_TRADE_FAKE_ALPACA_KEEP_HOME=1 Keep the runtime home after the test
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
  echo "error: jq is required for JSON provider validation" >&2
  exit 127
fi
if ! command -v curl >/dev/null 2>&1; then
  echo "error: curl is required for Unix-socket API validation" >&2
  exit 127
fi

if [[ -z "${home_dir}" ]]; then
  home_dir="$(mktemp -d "${TMPDIR:-/tmp}/mlai-trade-fake-alpaca.XXXXXX")"
else
  mkdir -p "${home_dir}"
fi

fake_pid=""

cleanup() {
  "${bin}" --home "${home_dir}" api stop >/dev/null 2>&1 || true
  "${bin}" --home "${home_dir}" daemon stop >/dev/null 2>&1 || true
  if [[ -n "${fake_pid}" ]]; then
    kill "${fake_pid}" >/dev/null 2>&1 || true
    wait "${fake_pid}" >/dev/null 2>&1 || true
  fi
  if [[ "${keep_home}" != "1" ]]; then
    rm -rf "${home_dir}"
  fi
}
trap cleanup EXIT

prepare_bootstrap_config() {
  mkdir -p "${home_dir}/config"
  jq '
    .daemon.enabled = true
    | .daemon.daily_refresh_enabled = false
    | .api.enabled = true
    | .auto.enabled = false
    | .feeds.sync_before_training = false
    | .feeds.sync_orders_before_training = false
  ' "${repo_root}/config/mlai-trade.example.json" >"${home_dir}/config/mlai-trade.json"
  cp "${repo_root}/config/tax-brackets.example.json" "${home_dir}/config/tax-brackets.json"
  chmod 600 "${home_dir}/config/mlai-trade.json" "${home_dir}/config/tax-brackets.json"
}

prepare_fake_provider_config() {
  local base_url="$1"
  jq --arg base "${base_url}" '
    .providers.alpaca.enabled = true
    | .alpaca.accounts = [
        {
          "_comment": "Fake Alpaca paper account for provider end-to-end tests.",
          "name": "paper-main",
          "enabled": true,
          "account_mode": "paper",
          "data_feed": "sip",
          "trading_base_url": $base,
          "data_base_url": $base,
          "api_key_id": "fake-key",
          "secret_key": "fake-secret"
        }
      ]
    | .fred.api_key = "fake-fred-key"
    | .daemon.enabled = true
    | .daemon.daily_refresh_enabled = false
    | .api.enabled = true
    | .auto.enabled = false
    | .auto.compliance.blocked_symbols = []
    | .feeds.sync_before_training = false
    | .feeds.sync_orders_before_training = false
    | .scan.max_concurrent = 1
  ' "${repo_root}/config/mlai-trade.example.json" >"${home_dir}/config/mlai-trade.json"
  chmod 600 "${home_dir}/config/mlai-trade.json"
}

start_fake_server() {
  local ready_file="${home_dir}/fake-alpaca-ready.json"
  local log_file="${home_dir}/logs/fake-alpaca.log"
  mkdir -p "${home_dir}/logs"
  : >"${ready_file}"
  "${bin}" --home "${home_dir}" runtime fake-alpaca-server --addr 127.0.0.1:0 \
    >"${ready_file}" 2>"${log_file}" &
  fake_pid="$!"

  local base_url=""
  for _ in $(seq 1 100); do
    if ! kill -0 "${fake_pid}" >/dev/null 2>&1; then
      echo "error: fake Alpaca server exited before readiness" >&2
      cat "${log_file}" >&2 || true
      exit 1
    fi
    base_url="$(jq -r 'select(.status == "ready") | .base_url // empty' "${ready_file}" 2>/dev/null || true)"
    if [[ -n "${base_url}" ]]; then
      printf '%s\n' "${base_url}"
      return
    fi
    sleep 0.1
  done
  echo "error: fake Alpaca server did not become ready" >&2
  cat "${log_file}" >&2 || true
  exit 1
}

run_plain() {
  label="$1"
  shift
  echo "==> ${label}"
  "$@" >/dev/null
}

run_json() {
  label="$1"
  shift
  echo "==> ${label}"
  output="$("$@")"
  printf '%s\n' "${output}" | jq empty
}

run_api_json() {
  label="$1"
  shift
  echo "==> ${label}"
  output="$(curl -s --unix-socket "${home_dir}/api/mlai-trade-api.sock" "$@")"
  printf '%s\n' "${output}" | jq empty
  printf '%s\n' "${output}" | jq -e '.ok != false' >/dev/null
}

prepare_bootstrap_config
base_url="$(start_fake_server)"
prepare_fake_provider_config "${base_url}"

run_json "fake account JSON" "${bin}" --home "${home_dir}" trade account --json
run_json "fake market clock JSON" "${bin}" --home "${home_dir}" market clock --json
run_json "fake market calendar JSON" "${bin}" --home "${home_dir}" market calendar --start 2026-05-01 --end 2026-05-01 --json
run_json "fake market quote AAPL JSON" "${bin}" --home "${home_dir}" market quote AAPL --json
run_plain "fake market bars AAPL" "${bin}" --home "${home_dir}" market bars AAPL --limit 5
run_json "fake market news AAPL JSON" "${bin}" --home "${home_dir}" market news AAPL --limit 3 --json
run_json "fake data-feed JSON" "${bin}" --home "${home_dir}" market data-feed --json
run_json "fake movers JSON" "${bin}" --home "${home_dir}" data movers --json
run_json "fake history start JSON" "${bin}" --home "${home_dir}" market history-start AAPL SPY --json

run_plain "fake universe sync" "${bin}" --home "${home_dir}" data universe
run_plain "fake bars scan" "${bin}" --home "${home_dir}" data scan --days 30 --force
run_json "fake data status JSON" "${bin}" --home "${home_dir}" data status --json
run_plain "fake screen" "${bin}" --home "${home_dir}" data screen --min-volume 1000
run_json "fake suggest JSON" "${bin}" --home "${home_dir}" data suggest --json
run_json "fake ml status JSON" "${bin}" --home "${home_dir}" ml status --json

run_plain "fake paper buy" "${bin}" --home "${home_dir}" trade buy AAPL 1 --account paper-main
run_json "fake positions after buy JSON" "${bin}" --home "${home_dir}" trade positions --account paper-main --json
run_json "fake orders after buy sync JSON" "${bin}" --home "${home_dir}" trade orders --account paper-main --sync --json
run_json "fake auto sync-orders JSON" "${bin}" --home "${home_dir}" auto sync-orders --json
run_plain "fake paper sell" "${bin}" --home "${home_dir}" trade sell AAPL 1 --account paper-main
run_json "fake orders after sell sync JSON" "${bin}" --home "${home_dir}" trade orders --account paper-main --sync --json
run_json "fake positions after sell JSON" "${bin}" --home "${home_dir}" trade positions --account paper-main --json
run_json "fake compliance tax account list JSON" "${bin}" --home "${home_dir}" compliance tax --accounts --json
run_json "fake compliance tax paper estimate JSON" "${bin}" --home "${home_dir}" compliance tax --year 2026 --account paper-main --details --json

run_plain "fake API start" "${bin}" --home "${home_dir}" api start
sleep 2
run_json "fake API status JSON" "${bin}" --home "${home_dir}" api status --details --json
run_api_json "fake API health" http://localhost/health
run_api_json "fake API trade account" http://localhost/trade/account?account=paper-main
run_api_json "fake API market quote" http://localhost/market/quote/AAPL
run_api_json "fake API market bars" http://localhost/market/bars/AAPL?limit=3
run_api_json "fake API trade buy" \
  -H 'content-type: application/json' \
  -d '{"qty":1,"account":"paper-main"}' \
  http://localhost/trade/buy/MSFT
run_api_json "fake API trade orders sync" http://localhost/trade/orders?account=paper-main\&sync=true
run_api_json "fake API trade sell" \
  -H 'content-type: application/json' \
  -d '{"qty":1,"account":"paper-main"}' \
  http://localhost/trade/sell/MSFT
run_api_json "fake API trade positions" http://localhost/trade/positions?account=paper-main
run_plain "fake API stop" "${bin}" --home "${home_dir}" api stop

echo "Fake Alpaca provider end-to-end test passed with home=${home_dir} base_url=${base_url}"
