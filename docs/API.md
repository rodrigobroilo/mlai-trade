# mlai-trade API

Last updated: 2026-05-03

The API is a local Unix-socket service for automation and dashboards. It returns JSON for every response. It does not expose `runtime` commands.

The API is intentionally local-only:

- It binds a Unix socket, not TCP.
- The socket file is created with `0600` permissions.
- Normal commands are synchronous and controlled by `api.request_timeout_seconds`.
- Long operations use `api.long_request_timeout_seconds`; today that applies to `ml refresh` and `feeds sync`.
- Underlying CLI actions run with `--json` and progress output disabled.

## Configuration

Runtime config:

```text
~/mlai-trade/config/mlai-trade.json
```

Example config:

```text
config/mlai-trade.example.json
```

API config block:

```json
{
  "api": {
    "enabled": false,
    "socket_file": "",
    "pid_file": "",
    "log_file": "",
    "request_timeout_seconds": 60,
    "long_request_timeout_seconds": 3600
  }
}
```

Defaults when fields are blank:

| Field | Default |
| --- | --- |
| `socket_file` | `~/mlai-trade/api/mlai-trade-api.sock` |
| `pid_file` | `~/mlai-trade/tmp/mlai-trade-api.pid` |
| `log_file` | `~/mlai-trade/logs/mlai-trade-api.log` |
| `request_timeout_seconds` | `60`, clamped to `5`-`300` |
| `long_request_timeout_seconds` | `3600`, clamped to `60`-`86400` |

API logs rotate daily. The active log remains `logs/mlai-trade-api.log`; archived logs are compressed as `logs/YYYYMMDD-mlai-trade-api.log.gz`.

## Lifecycle

The API refuses to start unless `api.enabled=true`.

```sh
mlai-trade api start
mlai-trade api status
mlai-trade api test
mlai-trade api reload
mlai-trade api restart
mlai-trade api stop
```

`api test` sends `GET /health` through the configured socket.

## Calling The API

Use `curl -s --unix-socket`:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock http://localhost/health
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock http://localhost/routes
```

Command routes use:

```text
GET  /{section}/{action}
POST /{section}/{action}
GET  /{section}/{action}/{target}
POST /{section}/{action}/{target}
```

Query parameters and JSON bodies are both accepted. For example:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  http://localhost/market/quote/AAPL

curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  -H 'content-type: application/json' \
  -d '{"symbol":"AAPL","timeframe":"1Day","limit":5}' \
  http://localhost/market/bars
```

Wrapped command responses look like:

```json
{
  "ok": true,
  "command": ["market", "quote", "AAPL"],
  "exit_code": 0,
  "duration_ms": 123,
  "data": {}
}
```

If a command prints non-JSON text, the API wraps it in `text`. Errors return JSON with `ok=false` and `error` or `stderr`.

Resource misses are errors. If the underlying command returns JSON with `ok:false`, the API response also uses `ok:false` and a non-2xx HTTP status even if the process exited cleanly. Commands can include `status_code` or `http_status` in their JSON to request a specific error status such as `404`.

Example miss:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  http://localhost/feeds/remove/MTA | jq
```

Expected shape:

```json
{
  "ok": false,
  "command": ["feeds", "remove", "MTA"],
  "exit_code": 1,
  "duration_ms": 24,
  "data": {
    "ok": false,
    "error": "MTA was not subscribed",
    "status_code": 404,
    "symbol": "MTA"
  },
  "error": "MTA was not subscribed"
}
```

## Allowlist

### Health

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/health` | API process health and socket status. |
| `GET` | `/routes` | Full allowlist as JSON. |

### Daemon

Only status and reload are exposed.

| Method | Path | Description |
| --- | --- | --- |
| `GET/POST` | `/daemon/status` | Daemon enabled/running/pid/log/interval status. |
| `GET/POST` | `/daemon/reload` | Send daemon reload signal. Fails if daemon is not running. |

### ML

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/ml/status` | none |
| `GET/POST` | `/ml/refresh` | `days`, `quick`, `backend`, `walk_forward_folds`, `top_n`, `slippage_bps` |
| `GET/POST` | `/ml/explain/{symbol}` | symbol in path or body/query |
| `GET/POST` | `/ml/explainable` | `limit` |
| `GET/POST` | `/ml/explained` | `limit` |

### Market

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/market/quote/{symbol}` | symbol in path or body/query |
| `GET/POST` | `/market/bars/{symbol}` | `timeframe`, `limit` |
| `GET/POST` | `/market/news/{symbol}` | optional symbol, `limit` |
| `GET/POST` | `/market/clock` | none |
| `GET/POST` | `/market/calendar` | `start`, `end`, `market` or `markets` |

### Trade

Read routes are always allowed. Mutation routes are allowed only when auto-trading is disabled.

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/trade/account` | optional `account`/`accounts` |
| `GET/POST` | `/trade/orders` | optional `account`/`accounts`, `status`, `limit`, `sync` |
| `GET/POST` | `/trade/positions` | optional `account`/`accounts` |
| `POST` | `/trade/buy/{symbol}` | required `qty`, required `account`/`accounts`, optional `type`, `limit_price`, `stop_price`, `tif` |
| `POST` | `/trade/sell/{symbol}` | required `qty`, required `account`/`accounts`, optional `type`, `limit_price`, `stop_price`, `tif` |
| `POST` | `/trade/cancel/{order_id}` | required `account`/`accounts` |
| `POST` | `/trade/close/{symbol}` | required `account`/`accounts` |

Examples:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  'http://localhost/trade/orders?account=alpaca:paper-main&sync=true&limit=20'

curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  -H 'content-type: application/json' \
  -d '{"qty":1,"account":"alpaca:paper-main","type":"market","tif":"day"}' \
  http://localhost/trade/buy/AAPL
```

### Data

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/data/status` | none |
| `GET/POST` | `/data/movers` | none |
| `GET/POST` | `/data/screen` | `min_volume` |
| `GET/POST` | `/data/watchlist` | none |
| `GET/POST` | `/data/suggest` | none |

### Compliance

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/compliance/wash` | none |
| `GET/POST` | `/compliance/pdt` | none |
| `GET/POST` | `/compliance/tax` | `accounts_list`, `details`, `show`, `show_brackets`, `year`, `quarter`, `export`, `account`/`accounts` |

Examples:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  'http://localhost/compliance/tax?year=2026'

curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  'http://localhost/compliance/tax?year=2026&quarter=1,2&details=true&account=alpaca:paper-main'
```

### Auto

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/auto/sync-orders` | none |
| `GET/POST` | `/auto/status` | none |
| `GET/POST` | `/auto/history` | `limit` |
| `GET/POST` | `/auto/config` | optional `key`, `value` |
| `GET/POST` | `/auto/config/{key}` | optional `value` |

The API does not expose `auto run`, `auto enable`, or `auto disable`.

### Feeds

Feed sync can be run directly, but `ml refresh` also reconciles the managed feed universe and syncs feeds before training when `feeds.sync_before_training=true`.

| Method | Path | Parameters |
| --- | --- | --- |
| `GET/POST` | `/feeds/add` | required `symbol` or `symbols` |
| `GET/POST` | `/feeds/remove/{symbol}` | symbol in path or body/query |
| `GET/POST` | `/feeds/sync` | `days` |
| `GET/POST` | `/feeds/list` | none |
| `GET/POST` | `/feeds/search/{query}` | query in path/body/query, optional `limit` |
| `GET/POST` | `/feeds/graph/{symbol}` | symbol in path or body/query |
| `GET/POST` | `/feeds/sentiment/{symbol}` | symbol in path or body/query |
| `GET/POST` | `/feeds/correlate` | `days` |
| `GET/POST` | `/feeds/status` | none |

Examples:

```sh
curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  -H 'content-type: application/json' \
  -d '{"symbols":["AAPL","NVDA"]}' \
  http://localhost/feeds/add

curl -s --unix-socket ~/mlai-trade/api/mlai-trade-api.sock \
  http://localhost/feeds/sentiment/NVDA
```

## Blocked Sections

`runtime` is not exposed. Requests such as `/runtime/version` return JSON with `ok=false`.

Unknown sections or non-allowlisted actions return `404` JSON errors.

All API logs are JSON lines in `logs/mlai-trade-api.log`. Request records include `method`, `path`, `command`, `status`, and `duration_ms`.
