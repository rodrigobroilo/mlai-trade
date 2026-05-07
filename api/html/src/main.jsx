import React, { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

const tabs = [
  ["overview", "Overview"],
  ["accounts", "Accounts"],
  ["positions", "Positions"],
  ["orders", "Orders"],
  ["auto", "Auto"],
  ["data", "Data"],
  ["compliance", "Compliance"],
];

const AUTO_REFRESH_MS = 30000;
const FULL_REFRESH_MS = 300000;

const defaultState = {
  accounts: null,
  positions: null,
  orders: null,
  auto: null,
  autoHistory: null,
  autoConfig: null,
  dataStatus: null,
  suggestions: null,
  watchlist: null,
  movers: null,
  wash: null,
  pdt: null,
  tax: null,
};

async function api(path, options = {}) {
  const controller = new AbortController();
  const timeoutMs = options.timeoutMs || 60000;
  const timer = window.setTimeout(() => controller.abort(), timeoutMs);
  try {
    const headers = { accept: "application/json", ...(options.headers || {}) };
    if (options.body && !headers["content-type"]) headers["content-type"] = "application/json";
    const res = await fetch(path, {
      ...options,
      headers,
      signal: controller.signal,
      body: options.body && typeof options.body !== "string" ? JSON.stringify(options.body) : options.body,
    });
    const text = await res.text();
    let json;
    try {
      json = text ? JSON.parse(text) : {};
    } catch (err) {
      if (!res.ok) throw new Error(text || res.statusText);
      throw err;
    }
    if (!res.ok || json.ok === false) {
      const message = json.error || json.reason || json.message || text || res.statusText;
      throw new Error(message);
    }
    return json;
  } finally {
    window.clearTimeout(timer);
  }
}

function dataOf(payload) {
  if (!payload) return {};
  const value = payload.data !== undefined ? payload.data : payload;
  return value ?? {};
}

function arrayFrom(value) {
  if (!value) return [];
  if (Array.isArray(value)) return value;
  if (Array.isArray(value.rows)) return value.rows;
  if (Array.isArray(value.results)) return value.results;
  if (Array.isArray(value.items)) return value.items;
  if (Array.isArray(value.subscriptions)) return value.subscriptions;
  if (Array.isArray(value.symbols)) return value.symbols;
  return [];
}

function objectFrom(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

function firstDefined(...values) {
  return values.find((value) => value !== undefined && value !== null && value !== "");
}

function number(value, fallback = 0) {
  const n = Number(value);
  return Number.isFinite(n) ? n : fallback;
}

function text(value, fallback = "not available") {
  if (value === undefined || value === null || value === "") return fallback;
  return String(value);
}

function money(value, compact = false) {
  const n = number(value);
  return n.toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    notation: compact && Math.abs(n) >= 100000 ? "compact" : "standard",
    maximumFractionDigits: compact && Math.abs(n) >= 100000 ? 1 : 2,
  });
}

function pct(value) {
  const n = number(value);
  return `${n >= 0 ? "+" : ""}${n.toFixed(1)}%`;
}

function tone(value) {
  const n = number(value);
  if (n > 0) return "gain";
  if (n < 0) return "loss";
  return "";
}

function dateText(value) {
  if (!value) return "not available";
  return String(value).replace("T", " ").replace("Z", "").slice(0, 19);
}

function dateInputValue(value = new Date()) {
  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function addDays(date, days) {
  const next = new Date(date);
  next.setDate(next.getDate() + days);
  return next;
}

function endOfDay(dateString) {
  const date = new Date(`${dateString}T23:59:59.999`);
  return Number.isNaN(date.getTime()) ? new Date() : date;
}

function chartDateLabel(value, compact = false) {
  if (!value) return "";
  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  return compact
    ? date.toLocaleDateString(undefined, { month: "short", day: "numeric" })
    : date.toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

function chartSpecFromRange(range) {
  const today = dateInputValue();
  const mode = range?.mode || "1d";
  const startDate =
    mode === "custom"
      ? range.start || today
      : mode === "7d"
        ? dateInputValue(addDays(new Date(), -6))
        : mode === "3d"
          ? dateInputValue(addDays(new Date(), -2))
          : today;
  const endDate = mode === "custom" ? range.end || today : today;
  const start = new Date(`${startDate}T00:00:00`);
  const end = endOfDay(endDate);
  const startDay = new Date(`${startDate}T00:00:00`);
  const endDay = new Date(`${endDate}T00:00:00`);
  const days = Math.max(1, Math.round((endDay - startDay) / 86400000) + 1);
  let timeframe = "1Min";
  let limit = 1000;
  if (days > 1 && days <= 3) {
    timeframe = "5Min";
  } else if (days > 3 && days <= 7) {
    timeframe = "15Min";
  } else if (days > 7 && days <= 30) {
    timeframe = "1Hour";
  } else if (days > 30) {
    timeframe = "1Day";
    limit = Math.min(1000, Math.max(90, days + 5));
  }
  const startIso = start.toISOString();
  const endIso = end.toISOString();
  return {
    mode,
    start,
    end,
    startDate,
    endDate,
    startIso,
    endIso,
    timeframe,
    limit,
    cacheKey: `${timeframe}:${limit}:${startIso}:${endIso}`,
    label: startDate === endDate ? startDate : `${startDate} to ${endDate}`,
  };
}

function normalizePct(value) {
  const n = number(value);
  return Math.abs(n) <= 1 ? n * 100 : n;
}

function providerName(account) {
  return text(account?.provider, "provider");
}

function accountRef(account) {
  return text(account?.account_ref || account?.name || account?.account_id, "account");
}

function accountSelector(account) {
  if (!account) return "not available";
  return account.selector || `${providerName(account)}:${accountRef(account)}`;
}

function accountObject(entry) {
  return entry?.account || entry || {};
}

function accountEntries(payload) {
  return arrayFrom(dataOf(payload).accounts || dataOf(payload));
}

function accountRows(payload) {
  return accountEntries(payload).map((entry) => {
    const account = accountObject(entry);
    return {
      ...entry,
      account,
      selector: accountSelector(account),
      provider: providerName(account),
      account_ref: accountRef(account),
      broker_account_id: account.broker_account_id,
      account_mode: account.account_mode,
      tax_universe: account.tax_universe,
      auto_trade_enabled: account.auto_trade_enabled,
      equity: entry.equity ?? entry.portfolio_value,
      cash: entry.cash,
      buying_power: entry.buying_power,
      day_pnl: entry.day_pnl,
      day_pnl_pct: entry.day_pnl_pct,
      pdt: entry.pattern_day_trader,
      trading_blocked: entry.trading_blocked,
      broker_status: entry.broker_status || entry.status,
      account_number: account.account_number,
      data_feed: account.data_feed,
    };
  });
}

function accountForEntry(entry) {
  return accountObject(entry);
}

function positionQty(row) {
  return number(row.qty ?? row.quantity ?? row.shares);
}

function positionCost(row) {
  return row.avg_entry_price ?? row.avg_cost ?? row.entry_price ?? row.entry ?? row.average_entry_price;
}

function positionCurrent(row) {
  return row.current_price ?? row.now ?? row.market_price ?? row.price;
}

function positionMarketValue(row) {
  return row.market_value ?? number(positionCurrent(row)) * positionQty(row);
}

function positionCostBasis(row) {
  return row.cost_basis ?? number(positionCost(row)) * positionQty(row);
}

function positionPnl(row) {
  return row.unrealized_pl ?? row.unrealized_pnl ?? row.pnl ?? number(positionMarketValue(row)) - number(positionCostBasis(row));
}

function positionPnlPct(row) {
  if (row.unrealized_plpc !== undefined) return normalizePct(row.unrealized_plpc);
  if (row.unrealized_pnl_pct !== undefined) return normalizePct(row.unrealized_pnl_pct);
  if (row.pnl_percent !== undefined) return normalizePct(row.pnl_percent);
  if (row.pnl_pct !== undefined) return normalizePct(row.pnl_pct);
  const basis = number(positionCostBasis(row));
  return basis ? (number(positionPnl(row)) / basis) * 100 : 0;
}

function positionOrigin(row) {
  return (
    row.management_origin_label ||
    row.execution_origin_label ||
    row.origin_label ||
    row.origin ||
    row.source ||
    row.provider ||
    "not available"
  );
}

function positionRows(payload) {
  const data = dataOf(payload);
  return accountEntries(data).flatMap((entry) => {
    const account = accountForEntry(entry);
    const selector = accountSelector(account);
    const provider = providerName(account);
    return arrayFrom(entry.positions).map((row) => ({
      ...row,
      account,
      account_selector: selector,
      provider,
      source: row.source || entry.provider_position_snapshot?.source || data.provider_query || "live",
    }));
  });
}

function orderRows(payload) {
  const data = dataOf(payload);
  return accountEntries(data).flatMap((entry) => {
    const account = accountForEntry(entry);
    const selector = accountSelector(account);
    return arrayFrom(entry.orders).map((order) => ({
      ...order,
      account,
      account_selector: selector,
      provider: providerName(account),
    }));
  });
}

function autoAccounts(payload) {
  return arrayFrom(dataOf(payload).accounts || dataOf(payload));
}

function autoManagedRows(payload) {
  return autoAccounts(payload).flatMap((entry) => {
    const account = {
      provider: entry.provider,
      account_ref: entry.account_ref,
      broker_account_id: entry.broker_account_id,
      account_mode: entry.account_mode,
      tax_universe: entry.tax_universe,
    };
    const selector = accountSelector(account);
    return arrayFrom(entry.auto_managed_positions || entry.positions).map((row) => ({
      ...row,
      account,
      account_selector: selector,
      provider: providerName(account),
      tracking_state: "auto_managed",
    }));
  });
}

function autoUnmanagedRows(payload) {
  return autoAccounts(payload).flatMap((entry) => {
    const account = {
      provider: entry.provider,
      account_ref: entry.account_ref,
      broker_account_id: entry.broker_account_id,
      account_mode: entry.account_mode,
      tax_universe: entry.tax_universe,
    };
    const selector = accountSelector(account);
    const rows = entry.unmanaged_positions || entry.provider_positions_not_tracked || entry.untracked_positions || [];
    return arrayFrom(rows).map((row) => ({
      ...row,
      account,
      account_selector: selector,
      provider: providerName(account),
      origin: row.origin || providerName(account),
      tracking_state: "not_tracked",
    }));
  });
}

function extractSuggestions(payload) {
  const data = dataOf(payload);
  return arrayFrom(data.suggestions || data.results || data);
}

function extractWatchlist(payload) {
  const data = dataOf(payload);
  return arrayFrom(data.results || data.watchlist || data);
}

function extractWash(payload) {
  const data = dataOf(payload);
  return arrayFrom(data.active || data.windows || data.rows || data);
}

function tradeHistoryRows(payload) {
  const data = dataOf(payload);
  return arrayFrom(data.trades || data.history || data.entries || data.rows || data);
}

function inChartRange(date, spec) {
  if (!spec || !date || Number.isNaN(date.getTime())) return true;
  return date >= spec.start && date <= spec.end;
}

function performanceValues(autoHistory, auto, chartSpec) {
  const closedTrades = tradeHistoryRows(autoHistory)
    .map((row) => ({
      ...row,
      chart_date: new Date(firstDefined(row.exit_timestamp_utc, row.exit_at, row.exit_date, row.date, "")),
    }))
    .filter((row) => Number.isFinite(number(row.pnl, NaN)) && inChartRange(row.chart_date, chartSpec))
    .sort((a, b) => a.chart_date - b.chart_date);
  let cumulative = 0;
  const values = [{ value: 0, date: chartSpec?.start || new Date() }];
  closedTrades.forEach((row) => {
    cumulative += number(row.pnl);
    values.push({ value: cumulative, date: row.chart_date });
  });
  const openPnl = autoManagedRows(auto).reduce((sum, row) => sum + number(positionPnl(row)), 0);
  if (openPnl || values.length < 2) values.push({ value: cumulative + openPnl, date: new Date() });
  return values;
}

function allocationRows(positions) {
  const total = positions.reduce((sum, row) => sum + Math.max(0, number(positionMarketValue(row))), 0);
  return positions
    .map((row) => ({
      ...row,
      allocation_value: Math.max(0, number(positionMarketValue(row))),
      allocation_pct: total ? (Math.max(0, number(positionMarketValue(row))) / total) * 100 : 0,
    }))
    .sort((a, b) => number(b.allocation_value) - number(a.allocation_value));
}

function barsFromPayload(payload) {
  const data = dataOf(payload);
  const bars = data.bars ?? data.rows ?? data;
  if (Array.isArray(bars)) return bars;
  if (bars && typeof bars === "object") return Object.values(bars).flat().filter(Boolean);
  return [];
}

function barDate(row) {
  const raw = firstDefined(row.t, row.timestamp, row.datetime, row.date, row.time);
  const date = raw ? new Date(raw) : null;
  return date && !Number.isNaN(date.getTime()) ? date : null;
}

function barClose(row) {
  return number(firstDefined(row.close, row.c, row.price), NaN);
}

function barCacheKey(symbol, chartSpec) {
  return `${text(symbol, "").toUpperCase()}:${chartSpec?.cacheKey || "default"}`;
}

function positionPnlSeries(row, barsBySymbol, chartSpec) {
  const symbol = text(row.symbol, "").toUpperCase();
  const bars = barsFromPayload(barsBySymbol[barCacheKey(symbol, chartSpec)]);
  const qty = positionQty(row);
  const cost = number(positionCost(row), NaN);
  const values = bars
    .map((bar) => ({ bar, date: barDate(bar) }))
    .filter(({ date }) => inChartRange(date, chartSpec))
    .sort((a, b) => a.date - b.date)
    .map((bar) => {
      const close = barClose(bar.bar);
      return Number.isFinite(close) && Number.isFinite(cost) ? { value: (close - cost) * qty, date: bar.date } : null;
    })
    .filter(Boolean);
  if (values.length >= 2) return values;
  return [
    { value: 0, date: chartSpec?.start || new Date() },
    { value: number(positionPnl(row)), date: new Date() },
  ];
}

function accountPnlSeries(account, positions, barsBySymbol, chartSpec) {
  const selector = account.selector || accountSelector(account.account || account);
  const series = positions
    .filter((row) => row.account_selector === selector)
    .map((row) => positionPnlSeries(row, barsBySymbol, chartSpec))
    .filter((values) => values.length >= 2);
  if (!series.length) {
    return [
      { value: 0, date: chartSpec?.start || new Date() },
      { value: number(account.day_pnl), date: new Date() },
    ];
  }
  const maxLen = Math.max(...series.map((values) => values.length));
  return Array.from({ length: maxLen }, (_, index) => {
    const longest = series.find((values) => values.length === maxLen) || series[0];
    return {
      date: longest[index]?.date || new Date(),
      value: series.reduce((sum, values) => {
        const offset = maxLen - values.length;
        const valueIndex = Math.max(0, index - offset);
        return sum + number(values[valueIndex]?.value);
      }, 0),
    };
  });
}

function taxDetailRows(payload) {
  return arrayFrom(dataOf(payload).details);
}

function washGroupKey(row) {
  return [
    text(firstDefined(row.tax_universe, row.universe, row.account_mode), "unknown"),
    text(row.symbol, "symbol").toUpperCase(),
    text(firstDefined(row.sell_date, String(row.sell_timestamp_utc || "").slice(0, 10)), "date"),
    text(firstDefined(row.wash_window_end, row.window_end), "window"),
  ].join("|");
}

function aggregateWashRows(rows) {
  const groups = new Map();
  rows.forEach((row) => {
    const key = washGroupKey(row);
    const existing = groups.get(key) || {
      ...row,
      account_refs: new Set(),
      sell_count: 0,
      loss_amount: 0,
      sell_price_total: 0,
    };
    if (row.account_ref) existing.account_refs.add(`${text(row.provider, "provider")}:${row.account_ref}`);
    existing.sell_count += 1;
    existing.loss_amount += number(row.loss_amount ?? row.loss);
    existing.sell_price_total += number(row.sell_price);
    existing.sell_price = existing.sell_count ? existing.sell_price_total / existing.sell_count : row.sell_price;
    existing.tax_universe = text(firstDefined(row.tax_universe, row.universe, row.account_mode), "unknown");
    existing.sell_date = firstDefined(row.sell_date, String(row.sell_timestamp_utc || "").slice(0, 10));
    existing.wash_window_end = firstDefined(row.wash_window_end, row.window_end);
    groups.set(key, existing);
  });
  return Array.from(groups.values())
    .map((row) => ({
      ...row,
      account_refs: Array.from(row.account_refs).sort().join(", "),
    }))
    .sort((a, b) =>
      String(b.wash_window_end || "").localeCompare(String(a.wash_window_end || "")) ||
      String(a.symbol || "").localeCompare(String(b.symbol || ""))
    );
}

function mlqIndex(auto) {
  const index = new Map();
  autoAccounts(auto).forEach((entry) => {
    const account = {
      provider: entry.provider,
      account_ref: entry.account_ref,
      broker_account_id: entry.broker_account_id,
      account_mode: entry.account_mode,
      tax_universe: entry.tax_universe,
    };
    const selector = accountSelector(account);
    const rows = [
      ...arrayFrom(entry.auto_managed_positions || entry.positions),
      ...arrayFrom(entry.provider_positions),
      ...arrayFrom(entry.unmanaged_positions || entry.provider_positions_not_tracked || entry.untracked_positions),
    ];
    rows.forEach((row) => {
      const symbol = text(row.symbol, "").toUpperCase();
      const mlq = firstDefined(row.ml_quintile, row.ml_quantile, row.mlq);
      if (!symbol || mlq === undefined) return;
      index.set(`${selector}:${symbol}`, mlq);
      index.set(`${entry.provider || account.provider}:${entry.account_ref || account.account_ref}:${symbol}`, mlq);
      if (!index.has(symbol)) index.set(symbol, mlq);
    });
  });
  return index;
}

function positionMlq(row, lookup) {
  const direct = firstDefined(row.ml_quintile, row.ml_quantile, row.mlq);
  if (direct !== undefined) return direct;
  const symbol = text(row.symbol, "").toUpperCase();
  if (!symbol || !lookup) return "-";
  const selector = row.account_selector || accountSelector(row.account);
  const account = row.account || {};
  return (
    lookup.get(`${selector}:${symbol}`) ||
    lookup.get(`${account.provider || row.provider}:${account.account_ref || row.account_ref}:${symbol}`) ||
    lookup.get(symbol) ||
    "-"
  );
}

function safeCellValue(col, row) {
  try {
    const value = col.value(row);
    return value === undefined || value === null || value === "" ? "-" : value;
  } catch (err) {
    return "-";
  }
}

function InfoTile({ label, value, detail, valueTone }) {
  return (
    <div className="info-tile">
      <span className="eyebrow">{label}</span>
      <strong className={valueTone || ""}>{value}</strong>
      <span>{detail || ""}</span>
    </div>
  );
}

function DataTable({ rows, columns, empty = "No rows." }) {
  const safeRows = Array.isArray(rows) ? rows : [];
  if (!safeRows.length) return <p className="muted">{empty}</p>;
  return (
    <div className="table-wrap">
      <table>
        <thead>
          <tr>
            {columns.map((col) => (
              <th key={col.label}>{col.label}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {safeRows.map((row, idx) => {
            const safeRow = row && typeof row === "object" && !Array.isArray(row) ? row : { value: row };
            return (
              <tr key={`${safeRow.account_selector || safeRow.account || ""}:${safeRow.symbol || safeRow.id || safeRow.path || idx}:${idx}`}>
                {columns.map((col) => (
                  <td key={col.label} className={col.className ? col.className(safeRow) : ""}>
                    {safeCellValue(col, safeRow)}
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function PagedDataTable({ rows, columns, empty, initial = 50, step = 50 }) {
  const [limit, setLimit] = useState(initial);
  const safeRows = Array.isArray(rows) ? rows : [];
  useEffect(() => {
    setLimit(initial);
  }, [safeRows.length, initial]);
  return (
    <div className="paged-table">
      <DataTable rows={safeRows.slice(0, limit)} columns={columns} empty={empty} />
      {safeRows.length > limit && (
        <button className="secondary" onClick={() => setLimit((current) => current + step)}>
          Show more +{step}
        </button>
      )}
      {safeRows.length > 0 && (
        <span className="table-count">
          Showing {Math.min(limit, safeRows.length)} of {safeRows.length}
        </span>
      )}
    </div>
  );
}

function JsonPanel({ title, value }) {
  return (
    <details className="json-panel">
      <summary>{title}</summary>
      <pre>{JSON.stringify(value ?? {}, null, 2)}</pre>
    </details>
  );
}

function PnlChart({ values, height = 260, compact = false }) {
  const ref = useRef(null);
  const pointsRef = useRef([]);
  const [hover, setHover] = useState(null);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    const ratio = window.devicePixelRatio || 1;
    const width = Math.max(1, Math.floor(canvas.clientWidth * ratio));
    const realHeight = Math.max(1, Math.floor(height * ratio));
    canvas.width = width;
    canvas.height = realHeight;
    ctx.clearRect(0, 0, width, realHeight);
    const series = Array.isArray(values)
      ? values
          .map((point) =>
            point && typeof point === "object"
              ? { value: number(point.value, NaN), date: point.date ? new Date(point.date) : null }
              : { value: number(point, NaN), date: null }
          )
          .filter((point) => Number.isFinite(point.value))
      : [];
    if (series.length < 2) {
      pointsRef.current = [];
      setHover(null);
      ctx.fillStyle = "#657287";
      ctx.font = `${(compact ? 11 : 13) * ratio}px system-ui`;
      ctx.textAlign = "center";
      ctx.fillText("No P&L series", width / 2, realHeight / 2);
      return;
    }

    const numericValues = series.map((point) => point.value);
    const min = Math.min(...numericValues, 0);
    const max = Math.max(...numericValues, 0);
    const span = Math.max(max - min, 0.0001);
    const padX = (compact ? 8 : 34) * ratio;
    const padTop = (compact ? 8 : 22) * ratio;
    const padBottom = (compact ? 19 : 30) * ratio;
    const chartWidth = width - padX * 2;
    const chartHeight = realHeight - padTop - padBottom;
    const yFor = (value) => realHeight - padBottom - ((value - min) / span) * chartHeight;
    const zeroY = yFor(0);
    const points = series.map((point, index) => [
      padX + (index / (series.length - 1)) * chartWidth,
      yFor(point.value),
      point.value,
      point.date,
    ]);
    pointsRef.current = points.map(([x, y, value, date]) => ({
      x: x / ratio,
      y: y / ratio,
      value,
      date,
    }));

    ctx.strokeStyle = "#dfe6ef";
    ctx.lineWidth = ratio;
    const gridLines = compact ? 2 : 5;
    for (let i = 0; i < gridLines; i += 1) {
      const y = padTop + (chartHeight * i) / Math.max(1, gridLines - 1);
      ctx.beginPath();
      ctx.moveTo(padX, y);
      ctx.lineTo(width - padX, y);
      ctx.stroke();
    }

    ctx.strokeStyle = "#8a5b5b";
    ctx.lineWidth = ratio;
    ctx.beginPath();
    ctx.moveTo(padX, zeroY);
    ctx.lineTo(width - padX, zeroY);
    ctx.stroke();

    const drawArea = (positive) => {
      const segments = points.filter(([, , value]) => (positive ? value >= 0 : value <= 0));
      if (segments.length < 2) return;
      ctx.beginPath();
      segments.forEach(([x, y], index) => (index === 0 ? ctx.moveTo(x, zeroY) : null));
      segments.forEach(([x, y], index) => (index === 0 ? ctx.lineTo(x, y) : ctx.lineTo(x, y)));
      [...segments].reverse().forEach(([x]) => ctx.lineTo(x, zeroY));
      ctx.closePath();
      ctx.fillStyle = positive ? "rgba(39, 184, 86, 0.18)" : "rgba(255, 75, 75, 0.16)";
      ctx.fill();
    };
    drawArea(true);
    drawArea(false);

    for (let i = 1; i < points.length; i += 1) {
      const [prevX, prevY, prevValue] = points[i - 1];
      const [x, y, value] = points[i];
      ctx.beginPath();
      ctx.moveTo(prevX, prevY);
      ctx.lineTo(x, y);
      ctx.strokeStyle = (prevValue + value) / 2 >= 0 ? "#1cb34f" : "#ff3f3f";
      ctx.lineWidth = (compact ? 2 : 3) * ratio;
      ctx.stroke();
    }

    const last = points[points.length - 1];
    ctx.fillStyle = last[2] >= 0 ? "#1cb34f" : "#ff3f3f";
    ctx.beginPath();
    ctx.arc(last[0], last[1], (compact ? 2.5 : 4) * ratio, 0, Math.PI * 2);
    ctx.fill();

    ctx.fillStyle = "#657287";
    ctx.font = `${(compact ? 9 : 11) * ratio}px system-ui`;
    ctx.textBaseline = "bottom";
    ctx.textAlign = "left";
    ctx.fillText(chartDateLabel(series[0].date, compact), padX, realHeight - 3 * ratio);
    ctx.textAlign = "right";
    ctx.fillText(chartDateLabel(series[series.length - 1].date, compact), width - padX, realHeight - 3 * ratio);

    if (!compact) {
      ctx.textBaseline = "alphabetic";
      ctx.textAlign = "left";
      ctx.fillText(money(max), padX, padTop - 6 * ratio);
      ctx.fillText(money(min), padX, realHeight - 16 * ratio);
    }
  }, [values, height, compact]);

  const handleMouseMove = (event) => {
    const points = pointsRef.current;
    if (!points.length) return;
    const rect = event.currentTarget.getBoundingClientRect();
    const x = event.clientX - rect.left;
    const nearest = points.reduce((best, point) => (Math.abs(point.x - x) < Math.abs(best.x - x) ? point : best), points[0]);
    const tooltipWidth = compact ? 132 : 158;
    setHover({
      ...nearest,
      left: Math.min(Math.max(nearest.x, 8), Math.max(8, rect.width - tooltipWidth - 8)),
      top: Math.max(8, nearest.y - (compact ? 46 : 54)),
      width: tooltipWidth,
    });
  };

  return (
    <div className={`chart-frame ${compact ? "mini" : ""}`} style={{ height }}>
      <canvas
        ref={ref}
        className={`chart pnl-chart ${compact ? "mini" : ""}`}
        style={{ height }}
        onMouseMove={handleMouseMove}
        onMouseLeave={() => setHover(null)}
      />
      {hover && (
        <div className={`chart-tooltip ${tone(hover.value)}`} style={{ left: hover.left, top: hover.top, width: hover.width }}>
          <strong>{money(hover.value)}</strong>
          <span>{chartDateLabel(hover.date)}</span>
        </div>
      )}
    </div>
  );
}

function AllocationBars({ rows, empty = "No allocation.", columns = false }) {
  if (!rows.length) return <p className="muted">{empty}</p>;
  return (
    <div className={`allocation-bars ${columns ? "two-column" : ""}`}>
      {rows.map((row) => (
        <div key={`${row.account_selector || ""}:${row.symbol}`} className="allocation-bar-row">
          <div>
            <strong>{text(row.symbol, "-")}</strong>
            <span>{row.account_ref || row.account?.account_ref || row.account_selector || ""}</span>
          </div>
          <div className="allocation-track">
            <span style={{ width: `${Math.max(1, Math.min(100, number(row.allocation_pct)))}%` }} />
          </div>
          <b>{pct(row.allocation_pct).replace("+", "")}</b>
          <em>{money(row.allocation_value, true)}</em>
        </div>
      ))}
    </div>
  );
}

function Sidebar({ activeTab, setActiveTab, accounts, status }) {
  const first = accounts[0];
  return (
    <aside className="sidebar" aria-label="Primary">
      <div className="brand">
        <span className="brand-mark">mt</span>
        <span>mlai-trade</span>
      </div>
      <nav className="nav" aria-label="Sections">
        {tabs.map(([id, label]) => (
          <button key={id} className={activeTab === id ? "active" : ""} onClick={() => setActiveTab(id)}>
            {label}
          </button>
        ))}
      </nav>
      <div className="sidebar-card">
        <span className="eyebrow">Primary account</span>
        <strong>{first ? first.selector : "Loading"}</strong>
        <span>{first ? `${first.provider} / ${first.account_mode || "account"}` : status}</span>
      </div>
    </aside>
  );
}

function MobileTabs({ activeTab, setActiveTab }) {
  return (
    <nav className="mobile-tabs" aria-label="Sections">
      {tabs.map(([id, label]) => (
        <button key={id} className={activeTab === id ? "active" : ""} onClick={() => setActiveTab(id)}>
          {label}
        </button>
      ))}
    </nav>
  );
}

function ChartRangeControls({ range, setRange }) {
  const today = dateInputValue();
  const setMode = (mode) => {
    if (mode === "custom") {
      setRange((current) => ({
        mode: "custom",
        start: current.start || today,
        end: current.end || today,
      }));
      return;
    }
    setRange({ mode, start: "", end: "" });
  };
  return (
    <div className="range-controls" aria-label="Chart date range">
      {[
        ["1d", "Today"],
        ["3d", "3 days"],
        ["7d", "7 days"],
        ["custom", "Range"],
      ].map(([mode, label]) => (
        <button key={mode} type="button" className={range.mode === mode ? "active" : ""} onClick={() => setMode(mode)}>
          {label}
        </button>
      ))}
      {range.mode === "custom" && (
        <>
          <input
            type="date"
            value={range.start || today}
            max={range.end || today}
            onChange={(event) => setRange((current) => ({ ...current, start: event.target.value }))}
            aria-label="Chart start date"
          />
          <input
            type="date"
            value={range.end || today}
            min={range.start || undefined}
            max={today}
            onChange={(event) => setRange((current) => ({ ...current, end: event.target.value }))}
            aria-label="Chart end date"
          />
        </>
      )}
    </div>
  );
}

function PositionTable({ rows, empty, mlqLookup, paged = false }) {
  const Table = paged ? PagedDataTable : DataTable;
  return (
    <Table
      rows={rows}
      empty={empty}
      columns={[
        { label: "Symbol", value: (r) => text(r.symbol, "-") },
        { label: "Origin", value: positionOrigin },
        { label: "Account", value: (r) => r.account_selector || accountSelector(r.account) },
        { label: "Qty", value: (r) => positionQty(r).toFixed(2) },
        { label: "Avg Cost", value: (r) => money(positionCost(r)) },
        { label: "Current", value: (r) => money(positionCurrent(r)) },
        { label: "Mkt Value", value: (r) => money(positionMarketValue(r)) },
        { label: "P&L", value: (r) => money(positionPnl(r)), className: (r) => tone(positionPnl(r)) },
        { label: "P&L%", value: (r) => pct(positionPnlPct(r)), className: (r) => tone(positionPnlPct(r)) },
        { label: "MLQ", value: (r) => positionMlq(r, mlqLookup) },
      ]}
    />
  );
}

function OrderTable({ rows, paged = false }) {
  const Table = paged ? PagedDataTable : DataTable;
  return (
    <Table
      rows={rows}
      columns={[
        { label: "Time", value: (r) => dateText(r.filled_at || r.submitted_at || r.created_at || r.time) },
        { label: "Account", value: (r) => r.account_selector || accountSelector(r.account) },
        { label: "Origin", value: (r) => r.execution_origin_label || r.origin || r.source || "-" },
        { label: "Symbol", value: (r) => text(r.symbol, "-") },
        { label: "Side", value: (r) => text(r.side, "-") },
        { label: "Qty", value: (r) => text(r.qty, "-") },
        { label: "Type", value: (r) => text(r.type, "-") },
        { label: "Status", value: (r) => text(r.status, "-") },
        { label: "Fill", value: (r) => (r.filled_avg_price ? money(r.filled_avg_price) : "-") },
      ]}
    />
  );
}

function AccountTable({ rows }) {
  return (
    <DataTable
      rows={rows}
      columns={[
        { label: "Selector", value: (r) => r.selector },
        { label: "Provider", value: (r) => r.provider },
        { label: "Mode", value: (r) => text(r.account_mode, "-") },
        { label: "Broker ID", value: (r) => text(r.broker_account_id, "-") },
        { label: "Auto", value: (r) => (r.auto_trade_enabled ? "enabled" : "disabled") },
        { label: "Equity", value: (r) => money(r.equity) },
        { label: "Cash", value: (r) => money(r.cash), className: (r) => tone(r.cash) },
        { label: "Buying Power", value: (r) => money(r.buying_power) },
        { label: "Day P&L", value: (r) => money(r.day_pnl), className: (r) => tone(r.day_pnl) },
        { label: "Status", value: (r) => text(r.broker_status, "-") },
      ]}
    />
  );
}

function Overview({ accounts, positions, orders, auto, autoHistory, mlqLookup, chartSpec }) {
  const managed = autoManagedRows(auto);
  const autoAccountsRows = autoAccounts(auto);
  const equity = accounts.reduce((sum, row) => sum + number(row.equity), 0);
  const cash = accounts.reduce((sum, row) => sum + number(row.cash), 0);
  const buyingPower = accounts.reduce((sum, row) => sum + number(row.buying_power), 0);
  const openValue = positions.reduce((sum, row) => sum + number(positionMarketValue(row)), 0);
  const unrealized = positions.reduce((sum, row) => sum + number(positionPnl(row)), 0);
  const autoPnl = managed.reduce((sum, row) => sum + number(positionPnl(row)), 0);
  const closedPnl = autoAccountsRows.reduce((sum, row) => sum + number(row.closed_pnl), 0);
  const perfValues = performanceValues(autoHistory, auto, chartSpec);
  const perfTrades = tradeHistoryRows(autoHistory).length;
  const allocation = allocationRows(positions);

  return (
    <div className="dashboard-grid">
      <article className="surface balance-panel">
        <div className="section-head">
          <div>
            <span className="eyebrow">Real trading performance</span>
            <h2>Auto realized + open P&L</h2>
          </div>
          <strong className={tone(perfValues[perfValues.length - 1]?.value)}>
            {perfValues.length ? money(perfValues[perfValues.length - 1]?.value) : "not available"}
          </strong>
        </div>
        <PnlChart values={perfValues} />
        <p className="chart-note">
          {chartSpec.label}: {perfTrades} total closed trades, plus {money(autoPnl)} current auto-managed open P&L.
        </p>
        <div className="performance-allocation">
          <div className="section-head compact">
            <h2>Allocation</h2>
            <span>{positions.length} provider positions</span>
          </div>
          <AllocationBars rows={allocation} empty="No open positions." columns />
        </div>
      </article>

      <aside className="side-column">
        <article className="surface metric-large">
          <span className="eyebrow">Total equity</span>
          <strong>{money(equity)}</strong>
          <span>{money(cash)} cash</span>
        </article>
        <article className="surface metric-large">
          <span className="eyebrow">Open market value</span>
          <strong>{money(openValue)}</strong>
          <span>{money(buyingPower)} buying power</span>
        </article>
      </aside>

      <section className="metrics-row" aria-label="Account metrics">
        <article className="metric-tile">
          <span className="eyebrow">Accounts</span>
          <strong>{accounts.length}</strong>
        </article>
        <article className="metric-tile">
          <span className="eyebrow">Provider positions</span>
          <strong>{positions.length}</strong>
        </article>
        <article className="metric-tile">
          <span className="eyebrow">Unrealized P&L</span>
          <strong className={tone(unrealized)}>{money(unrealized)}</strong>
        </article>
        <article className="metric-tile">
          <span className="eyebrow">Auto P&L</span>
          <strong className={tone(autoPnl + closedPnl)}>{money(autoPnl + closedPnl)}</strong>
        </article>
      </section>

      <article className="surface table-panel wide-panel">
        <div className="section-head">
          <div>
            <span className="eyebrow">Provider source</span>
            <h2>Open Positions</h2>
          </div>
          <span className="status-pill">{positions.length}</span>
        </div>
        <PositionTable rows={positions.slice(0, 18)} mlqLookup={mlqLookup} />
      </article>

      <article className="surface table-panel wide-panel">
        <div className="section-head">
          <div>
            <span className="eyebrow">Execution source</span>
            <h2>Recent Orders</h2>
          </div>
          <span className="status-pill">{orders.length}</span>
        </div>
        <OrderTable rows={orders.slice(0, 18)} />
      </article>
    </div>
  );
}

function AccountPerformanceCards({ accounts, positions, barsBySymbol, chartSpec }) {
  return (
    <div className="account-card-grid">
      {accounts.map((account) => {
        const accountPositions = positions.filter((row) => row.account_selector === account.selector);
        const allocation = allocationRows(accountPositions);
        const pnlSeries = accountPnlSeries(account, positions, barsBySymbol, chartSpec);
        const current = pnlSeries[pnlSeries.length - 1]?.value ?? number(account.day_pnl);
        return (
          <article className="surface account-performance-card" key={account.selector}>
            <div className="section-head compact">
              <div>
                <span className="eyebrow">{account.provider}</span>
                <h2>{account.selector}</h2>
              </div>
              <strong className={tone(current)}>{money(current)}</strong>
            </div>
            <PnlChart values={pnlSeries} height={150} compact />
            <AllocationBars rows={allocation} empty="No open positions." />
          </article>
        );
      })}
    </div>
  );
}

function AccountsView({ rows, raw, positions, barsBySymbol, chartSpec }) {
  return (
    <div className="section-layout">
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Providers</span>
            <h2>Accounts</h2>
          </div>
          <span className="status-pill">{rows.length}</span>
        </div>
        <AccountTable rows={rows} />
      </article>
      <AccountPerformanceCards accounts={rows} positions={positions} barsBySymbol={barsBySymbol} chartSpec={chartSpec} />
      <JsonPanel title="Raw account API response" value={raw} />
    </div>
  );
}

function PositionChartGrid({ rows, barsBySymbol, mlqLookup, chartSpec }) {
  const [limit, setLimit] = useState(50);
  const safeRows = Array.isArray(rows) ? rows : [];
  useEffect(() => setLimit(50), [safeRows.length]);
  return (
    <article className="surface">
      <div className="section-head">
        <div>
          <span className="eyebrow">Per-position P&L</span>
          <h2>Open Position Charts</h2>
        </div>
        <span className="status-pill">{safeRows.length}</span>
      </div>
      <div className="position-chart-grid">
        {safeRows.slice(0, limit).map((row) => {
          const values = positionPnlSeries(row, barsBySymbol, chartSpec);
          const current = number(positionPnl(row));
          return (
            <article className="position-card" key={`${row.account_selector}:${row.symbol}`}>
              <div className="position-card-head">
                <div>
                  <strong>{text(row.symbol, "-")}</strong>
                  <span>{row.account_selector}</span>
                </div>
                <b className={tone(current)}>{money(current)}</b>
              </div>
              <PnlChart values={values} height={130} compact />
              <div className="position-card-stats">
                <span>Qty {positionQty(row).toFixed(2)}</span>
                <span>MLQ {positionMlq(row, mlqLookup)}</span>
                <span className={tone(positionPnlPct(row))}>{pct(positionPnlPct(row))}</span>
              </div>
            </article>
          );
        })}
      </div>
      {safeRows.length > limit && (
        <button className="secondary" onClick={() => setLimit((current) => current + 50)}>
          Show more +50
        </button>
      )}
    </article>
  );
}

function PositionsView({ positions, auto, mlqLookup, barsBySymbol, chartSpec }) {
  const managed = autoManagedRows(auto);
  const unmanaged = autoUnmanagedRows(auto);
  return (
    <div className="section-layout">
      <PositionChartGrid rows={positions} barsBySymbol={barsBySymbol} mlqLookup={mlqLookup} chartSpec={chartSpec} />
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Provider query</span>
            <h2>All Open Positions</h2>
          </div>
          <span className="status-pill">{positions.length}</span>
        </div>
        <PositionTable rows={positions} mlqLookup={mlqLookup} paged />
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Auto rules</span>
            <h2>Tracked vs Not Tracked</h2>
          </div>
          <span className="status-pill">{managed.length} tracked / {unmanaged.length} not tracked</span>
        </div>
        <PositionTable rows={[...managed, ...unmanaged]} mlqLookup={mlqLookup} paged />
      </article>
    </div>
  );
}

function OrdersView({ rows, syncOrders }) {
  return (
    <div className="section-layout">
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Provider orders</span>
            <h2>Recent Orders</h2>
          </div>
          <button className="primary" onClick={syncOrders}>
            Sync orders
          </button>
        </div>
        <OrderTable rows={rows} paged />
      </article>
    </div>
  );
}

function AutoView({ auto, autoHistory, autoConfig, mlqLookup }) {
  const data = dataOf(auto);
  const accounts = autoAccounts(auto);
  const managed = autoManagedRows(auto);
  const unmanaged = autoUnmanagedRows(auto);
  const enabled = data.enabled ?? accounts.some((a) => a.auto_trade_enabled);
  return (
    <div className="section-layout">
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Rules</span>
            <h2>Auto Trading</h2>
          </div>
          <span className="status-pill">{enabled ? "enabled" : "disabled"}</span>
        </div>
        <div className="auto-grid">
          <InfoTile label="Accounts" value={String(accounts.length)} detail="configured accounts" />
          <InfoTile label="Auto-managed" value={String(managed.length)} detail={`${unmanaged.length} provider positions outside auto`} />
          <InfoTile label="Max positions" value={text(data.config?.max_positions || accounts[0]?.max_positions, "not available")} detail="per account" />
          <InfoTile label="History rows" value={String(arrayFrom(dataOf(autoHistory).history || dataOf(autoHistory)).length)} detail="auto cycle history" />
        </div>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Tracked by auto rules</span>
            <h2>Auto-managed Positions</h2>
          </div>
        </div>
        <PositionTable rows={managed} empty="No auto-managed positions." mlqLookup={mlqLookup} />
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Manual or provider-held</span>
            <h2>Positions Not Tracked By Auto</h2>
          </div>
        </div>
        <PositionTable rows={unmanaged} empty="No provider positions outside auto tracking." mlqLookup={mlqLookup} />
      </article>
      <JsonPanel title="Auto configuration" value={dataOf(autoConfig)} />
    </div>
  );
}

function DataView({ status, suggestions, watchlist, movers }) {
  const s = dataOf(status);
  const suggestionRows = extractSuggestions(suggestions);
  const watchRows = extractWatchlist(watchlist);
  const moverRows = arrayFrom(dataOf(movers).movers || dataOf(movers).results || dataOf(movers));
  return (
    <div className="section-layout">
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Data pipeline</span>
            <h2>Status</h2>
          </div>
        </div>
        <div className="ml-grid">
          <InfoTile label="Assets" value={text(s.assets?.symbols ?? s.assets, "not available")} detail="tradable universe" />
          <InfoTile label="Bars" value={text(s.bars?.rows ?? s.bars, "not available")} detail={text(s.bars?.range, "")} />
          <InfoTile label="Feeds" value={text(s.feeds?.subscriptions ?? s.feeds, "not available")} detail={`${text(s.feeds?.articles, "not available")} articles`} />
          <InfoTile label="DB size" value={s.db_size_bytes ? money(s.db_size_bytes / 1000000000, true).replace("$", "") + " GB" : "not available"} detail="SQLite file" />
        </div>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">ML and feed scoring</span>
            <h2>Suggestions</h2>
          </div>
          <span className="status-pill">{suggestionRows.length}</span>
        </div>
        <DataTable
          rows={suggestionRows.slice(0, 50)}
          columns={[
            { label: "Rank", value: (r) => text(r.rank, "-") },
            { label: "Symbol", value: (r) => text(r.symbol, "-") },
            { label: "Score", value: (r) => text(r.score, "-") },
            { label: "Confidence", value: (r) => text(r.confidence, "-") },
            { label: "Close", value: (r) => money(r.close) },
            { label: "Change", value: (r) => pct(r.change_pct), className: (r) => tone(r.change_pct) },
            { label: "Signals", value: (r) => arrayFrom(r.signals).join(", ") || "-" },
          ]}
        />
      </article>
      <div className="section-layout two-columns">
        <article className="surface">
          <div className="section-head">
            <h2>Watchlist</h2>
            <span className="status-pill">{watchRows.length}</span>
          </div>
          <DataTable rows={watchRows.slice(0, 50)} columns={[{ label: "Symbol", value: (r) => text(r.symbol, "-") }, { label: "Score", value: (r) => text(r.score, "-") }, { label: "Confidence", value: (r) => text(r.confidence, "-") }]} />
        </article>
        <article className="surface">
          <div className="section-head">
            <h2>Movers</h2>
            <span className="status-pill">{moverRows.length}</span>
          </div>
          <DataTable rows={moverRows.slice(0, 50)} columns={[{ label: "Symbol", value: (r) => text(r.symbol, "-") }, { label: "Price", value: (r) => money(r.price ?? r.close) }, { label: "Change", value: (r) => pct(r.change_pct ?? r.percent_change), className: (r) => tone(r.change_pct ?? r.percent_change) }]} />
        </article>
      </div>
    </div>
  );
}

function ComplianceView({ wash, pdt, tax, taxError, taxYear, setTaxYear, taxAccount, setTaxAccount, accounts, loadTax }) {
  const washRows = aggregateWashRows(extractWash(wash));
  const paperWashRows = washRows.filter((row) => row.tax_universe === "paper");
  const realWashRows = washRows.filter((row) => row.tax_universe !== "paper");
  const pdtData = dataOf(pdt);
  const taxData = dataOf(tax);
  const taxSummary = taxData.consolidated || arrayFrom(taxData.by_account)[0] || taxData;
  const taxAmount = taxSummary.estimated_federal_tax || {};
  const details = taxDetailRows(tax);
  const washColumns = [
    { label: "Symbol", value: (r) => text(r.symbol, "-") },
    { label: "Sold", value: (r) => dateText(firstDefined(r.sell_date, r.sell_timestamp_utc, r.sold_at, r.sold_date, r.date)).slice(0, 10) },
    { label: "Accounts", value: (r) => text(r.account_refs || r.account_ref, "-") },
    { label: "Events", value: (r) => text(r.sell_count, "1") },
    { label: "Loss", value: (r) => money(r.loss_amount ?? r.loss) },
    { label: "Window End", value: (r) => dateText(firstDefined(r.wash_window_end, r.window_end, r.window_end_date, r.expires_at, r.expiration_date)).slice(0, 10) },
    { label: "Universe", value: (r) => text(firstDefined(r.tax_universe, r.universe, r.account_mode), "-") },
  ];
  return (
    <div className="section-layout">
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Wash sale and PDT</span>
            <h2>Compliance</h2>
          </div>
        </div>
        <div className="auto-grid">
          <InfoTile label="Wash windows" value={text(dataOf(wash).active_count ?? washRows.length, "0")} detail={`${paperWashRows.length} paper / ${realWashRows.length} real grouped rows`} />
          <InfoTile label="Day trades" value={text(pdtData.day_trades_5d ?? pdtData.day_trades, "not available")} detail="rolling 5 business days" />
          <InfoTile label="PDT flag" value={text(pdtData.pattern_day_trader ?? pdtData.alpaca_pdt_flag, "not available")} detail="provider status" />
          <InfoTile label="Remaining" value={text(pdtData.remaining_day_trades, "not available")} detail="before PDT trigger" />
        </div>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">IRS 1091</span>
            <h2>Active Wash Sale Windows - Paper</h2>
          </div>
          <span className="status-pill">{paperWashRows.length}</span>
        </div>
        <PagedDataTable rows={paperWashRows} columns={washColumns} empty="No active paper wash-sale windows." />
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">IRS 1091</span>
            <h2>Active Wash Sale Windows - Real</h2>
          </div>
          <span className="status-pill">{realWashRows.length}</span>
        </div>
        <PagedDataTable rows={realWashRows} columns={washColumns} empty="No active real wash-sale windows." />
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Federal estimate</span>
            <h2>Tax</h2>
          </div>
          <form
            className="symbol-form tax-form"
            onSubmit={(event) => {
              event.preventDefault();
              loadTax(taxYear, taxAccount);
            }}
          >
            <input value={taxYear} onChange={(event) => setTaxYear(event.target.value.replace(/\D/g, "").slice(0, 4))} aria-label="Tax year" />
            <select value={taxAccount} onChange={(event) => setTaxAccount(event.target.value)} aria-label="Tax account">
              <option value="">Default real accounts</option>
              {accounts.map((account) => (
                <option key={account.selector} value={account.selector}>
                  {account.selector}
                </option>
              ))}
            </select>
            <button className="primary">Load</button>
          </form>
        </div>
        <div className="auto-grid">
          <InfoTile label="Year" value={text(taxSummary.year, taxYear)} detail={text(taxSummary.period_label, "")} />
          <InfoTile label="Short-term" value={money(taxAmount.short_term ?? taxSummary.short_term_tax ?? taxSummary.short_tax)} detail={money(taxSummary.short_term?.net ?? taxSummary.short_term_net ?? taxSummary.short_net)} />
          <InfoTile label="Long-term" value={money(taxAmount.long_term ?? taxSummary.long_term_tax ?? taxSummary.long_tax)} detail={money(taxSummary.long_term?.net ?? taxSummary.long_term_net ?? taxSummary.long_net)} />
          <InfoTile label="Total tax" value={money(taxAmount.total ?? taxSummary.total_tax ?? taxSummary.estimated_federal_tax)} detail={text(taxSummary.filing_status_label || taxSummary.filing_status, "")} />
        </div>
        {taxError && <p className="error-text">{taxError}</p>}
        <div className="section-head compact tax-details-head">
          <div>
            <span className="eyebrow">Operation details</span>
            <h2>Taxable Operations</h2>
          </div>
          <span className="status-pill">{details.length}</span>
        </div>
        <PagedDataTable
          rows={details}
          empty="No tax details loaded. Select an account and press Load."
          columns={[
            { label: "Exit", value: (r) => dateText(r.exit_date).slice(0, 10) },
            { label: "Account", value: (r) => `${text(r.provider, "-")}:${text(r.account_ref, "-")}` },
            { label: "Origin", value: (r) => text(r.execution_origin, "-") },
            { label: "Symbol", value: (r) => text(r.symbol, "-") },
            { label: "Qty", value: (r) => number(r.qty).toFixed(2) },
            { label: "Term", value: (r) => text(r.term, "-") },
            { label: "P&L", value: (r) => money(r.pnl), className: (r) => tone(r.pnl) },
            { label: "Tax Impact", value: (r) => money(r.estimated_federal_tax_impact), className: (r) => tone(r.estimated_federal_tax_impact) },
          ]}
        />
        <JsonPanel title="Raw tax estimate" value={taxData} />
      </article>
    </div>
  );
}

class ErrorBoundary extends React.Component {
  constructor(props) {
    super(props);
    this.state = { error: null };
  }

  static getDerivedStateFromError(error) {
    return { error };
  }

  componentDidCatch(error, info) {
    console.error("dashboard render failed", error, info);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="error-page">
        <article className="surface">
          <span className="eyebrow">Dashboard error</span>
          <h1>The live dashboard could not render this refresh.</h1>
          <p className="muted">{this.state.error.message || String(this.state.error)}</p>
          <button className="primary" onClick={() => window.location.reload()}>
            Reload dashboard
          </button>
        </article>
      </div>
    );
  }
}

function App() {
  const [activeTab, setActiveTab] = useState("overview");
  const [state, setState] = useState(defaultState);
  const [errors, setErrors] = useState({});
  const [status, setStatus] = useState("Loading snapshot");
  const [taxYear, setTaxYear] = useState(String(new Date().getFullYear()));
  const [taxAccount, setTaxAccount] = useState("");
  const [chartRange, setChartRange] = useState({ mode: "1d", start: "", end: "" });
  const [positionBars, setPositionBars] = useState({});
  const refreshInFlight = useRef(false);
  const barsInFlight = useRef(new Set());
  const taxYearRef = useRef(taxYear);
  const taxAccountRef = useRef(taxAccount);

  const accounts = useMemo(() => accountRows(state.accounts), [state.accounts]);
  const positions = useMemo(() => positionRows(state.positions), [state.positions]);
  const orders = useMemo(() => orderRows(state.orders), [state.orders]);
  const mlqLookup = useMemo(() => mlqIndex(state.auto), [state.auto]);
  const chartSpec = useMemo(() => chartSpecFromRange(chartRange), [chartRange]);

  useEffect(() => {
    taxYearRef.current = taxYear;
  }, [taxYear]);

  useEffect(() => {
    taxAccountRef.current = taxAccount;
  }, [taxAccount]);

  function setResource(key, value) {
    setState((current) => ({ ...current, [key]: value }));
  }

  function setResourceError(key, err) {
    setErrors((current) => ({ ...current, [key]: err.message || String(err) }));
  }

  function clearResourceError(key) {
    setErrors((current) => {
      if (!current[key]) return current;
      const next = { ...current };
      delete next[key];
      return next;
    });
  }

  async function loadResource(key, path, options) {
    try {
      const payload = await api(path, options);
      setResource(key, payload);
      clearResourceError(key);
      return payload;
    } catch (err) {
      setResourceError(key, err);
      return null;
    }
  }

  async function loadTax(year = taxYear, account = taxAccount) {
    if (!/^\d{4}$/.test(year)) return;
    setStatus(`Loading tax ${year}`);
    const params = new URLSearchParams({ year, details: "true" });
    if (account) params.set("account", account);
    await loadResource("tax", `/compliance/tax?${params.toString()}`, { timeoutMs: 180000 });
    setStatus(`Loaded tax ${year}`);
  }

  async function syncOrders() {
    setStatus("Syncing provider orders");
    await loadResource("syncOrders", "/auto/sync-orders", { timeoutMs: 180000 });
    await Promise.all([
      loadResource("orders", "/trade/orders?limit=100&sync=true", { timeoutMs: 180000 }),
      loadResource("positions", "/trade/positions?sync=true", { timeoutMs: 180000 }),
      loadResource("auto", "/auto/status"),
      loadResource("wash", "/compliance/wash"),
      loadResource("pdt", "/compliance/pdt"),
    ]);
    setStatus(`Synced ${new Date().toLocaleTimeString()}`);
  }

  async function refreshAll({ silent = false, full = false } = {}) {
    if (refreshInFlight.current) return;
    refreshInFlight.current = true;
    if (!silent) setStatus("Loading latest snapshot");
    const requests = [
      ["accounts", "/trade/account"],
      ["positions", "/trade/positions?sync=false", { timeoutMs: 180000 }],
      ["orders", "/trade/orders?limit=100&sync=false", { timeoutMs: 180000 }],
      ["auto", "/auto/status"],
      ["autoHistory", "/auto/history"],
      ["autoConfig", "/auto/config"],
      ["wash", "/compliance/wash"],
      ["pdt", "/compliance/pdt"],
    ];
    if (full) {
      const taxParams = new URLSearchParams({ year: taxYearRef.current, details: "true" });
      if (taxAccountRef.current) taxParams.set("account", taxAccountRef.current);
      requests.push(
        ["dataStatus", "/data/status"],
        ["suggestions", "/data/suggest"],
        ["watchlist", "/data/watchlist"],
        ["movers", "/data/movers"],
        ["tax", `/compliance/tax?${taxParams.toString()}`, { timeoutMs: 180000 }]
      );
    }
    try {
      await Promise.all(requests.map(([key, path, options]) => loadResource(key, path, options)));
      const errorCount = Object.keys(errors).length;
      setStatus(`${errorCount ? `${errorCount} errors, ` : ""}Updated ${new Date().toLocaleTimeString()}`);
    } finally {
      refreshInFlight.current = false;
    }
  }

  useEffect(() => {
    refreshAll({ full: true }).catch((err) => setStatus(err.message));
    const liveTimer = window.setInterval(() => {
      refreshAll({ silent: true }).catch((err) => setStatus(err.message));
    }, AUTO_REFRESH_MS);
    const fullTimer = window.setInterval(() => {
      refreshAll({ silent: true, full: true }).catch((err) => setStatus(err.message));
    }, FULL_REFRESH_MS);
    return () => {
      window.clearInterval(liveTimer);
      window.clearInterval(fullTimer);
    };
    // Run once on first load, then refresh read-only dashboard routes automatically.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const symbols = Array.from(new Set(positions.map((row) => text(row.symbol, "").toUpperCase()).filter(Boolean)));
    const missing = symbols.filter((symbolName) => {
      const key = barCacheKey(symbolName, chartSpec);
      return !positionBars[key] && !barsInFlight.current.has(key);
    });
    if (!missing.length) return undefined;
    let cancelled = false;
    async function loadMissingBars() {
      const workers = Array.from({ length: Math.min(4, missing.length) }, async (_, workerIndex) => {
        for (let index = workerIndex; index < missing.length; index += 4) {
          const symbolName = missing[index];
          const key = barCacheKey(symbolName, chartSpec);
          barsInFlight.current.add(key);
          try {
            const params = new URLSearchParams({
              timeframe: chartSpec.timeframe,
              limit: String(chartSpec.limit),
              start: chartSpec.startIso,
              end: chartSpec.endIso,
            });
            const payload = await api(`/market/bars/${encodeURIComponent(symbolName)}?${params.toString()}`, { timeoutMs: 60000 });
            if (!cancelled) {
              setPositionBars((current) => ({ ...current, [key]: payload }));
            }
          } catch (err) {
            setResourceError(`bars:${symbolName}`, err);
          } finally {
            barsInFlight.current.delete(key);
          }
        }
      });
      await Promise.all(workers);
    }
    loadMissingBars();
    return () => {
      cancelled = true;
    };
  }, [positions, positionBars, chartSpec]);

  return (
    <div className="app-shell">
      <Sidebar activeTab={activeTab} setActiveTab={setActiveTab} accounts={accounts} status={status} />
      <div className="workspace">
        <header className="topbar">
          <div>
            <span className="eyebrow">Trading snapshot dashboard</span>
            <h1>mlai-trade</h1>
          </div>
          <div className="toolbar">
            <ChartRangeControls range={chartRange} setRange={setChartRange} />
            <span className="status-pill">Bars {chartSpec.timeframe}</span>
            <span className="status-pill">Snapshot every {AUTO_REFRESH_MS / 1000}s</span>
            <span className="status-pill">{status}</span>
          </div>
        </header>
        <MobileTabs activeTab={activeTab} setActiveTab={setActiveTab} />
        <main>
          <section className={`panel ${activeTab === "overview" ? "active" : ""}`}>
            <Overview
              accounts={accounts}
              positions={positions}
              orders={orders}
              auto={state.auto}
              autoHistory={state.autoHistory}
              mlqLookup={mlqLookup}
              chartSpec={chartSpec}
            />
          </section>
          <section className={`panel ${activeTab === "accounts" ? "active" : ""}`}>
            <AccountsView rows={accounts} raw={state.accounts} positions={positions} barsBySymbol={positionBars} chartSpec={chartSpec} />
          </section>
          <section className={`panel ${activeTab === "positions" ? "active" : ""}`}>
            <PositionsView positions={positions} auto={state.auto} mlqLookup={mlqLookup} barsBySymbol={positionBars} chartSpec={chartSpec} />
          </section>
          <section className={`panel ${activeTab === "orders" ? "active" : ""}`}>
            <OrdersView rows={orders} syncOrders={syncOrders} />
          </section>
          <section className={`panel ${activeTab === "auto" ? "active" : ""}`}>
            <AutoView auto={state.auto} autoHistory={state.autoHistory} autoConfig={state.autoConfig} mlqLookup={mlqLookup} />
          </section>
          <section className={`panel ${activeTab === "data" ? "active" : ""}`}>
            <DataView status={state.dataStatus} suggestions={state.suggestions} watchlist={state.watchlist} movers={state.movers} />
          </section>
          <section className={`panel ${activeTab === "compliance" ? "active" : ""}`}>
            <ComplianceView
              wash={state.wash}
              pdt={state.pdt}
              tax={state.tax}
              taxError={errors.tax}
              taxYear={taxYear}
              setTaxYear={setTaxYear}
              taxAccount={taxAccount}
              setTaxAccount={setTaxAccount}
              accounts={accounts}
              loadTax={loadTax}
            />
          </section>
        </main>
      </div>
    </div>
  );
}

createRoot(document.getElementById("root")).render(
  <ErrorBoundary>
    <App />
  </ErrorBoundary>
);
