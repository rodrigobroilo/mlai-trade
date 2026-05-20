import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

const tabs = [
  ["overview", "Overview"],
  ["accounts", "Accounts"],
  ["positions", "Positions"],
  ["orders", "Orders"],
  ["data", "Data"],
  ["compliance", "Compliance"],
];
const DEFAULT_TAB = "overview";
const tabIds = new Set(tabs.map(([id]) => id));
const DASHBOARD_TAB_STORAGE_KEY = "mlai-trade-dashboard-tab";
const DASHBOARD_ACCOUNT_STORAGE_KEY = "mlai-trade-dashboard-account";

const AUTO_REFRESH_MS = 60000;
const FULL_REFRESH_MS = 300000;
const API_CLIENT_CONCURRENCY = 4;
const POSITION_BAR_WORKERS = 2;
const API_MAX_RETRIES = 2;
const MARKET_BARS_FALLBACK_BATCH_SIZE = 25;
const DASHBOARD_ORDERS_FALLBACK_LIMIT = 100;
const DASHBOARD_TABLE_FALLBACK_INITIAL_ROWS = 50;
const DASHBOARD_TABLE_FALLBACK_PAGE_ROWS = 50;
const DASHBOARD_DATA_FALLBACK_INITIAL_ROWS = 20;
const DASHBOARD_DATA_FALLBACK_PAGE_ROWS = 20;
const CLIENT_LOCALE = typeof navigator === "undefined" ? undefined : navigator.language || undefined;
const CLIENT_TIME_ZONE = (() => {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
})();

let apiActiveRequests = 0;
const apiQueue = [];

const defaultState = {
  accounts: null,
  positions: null,
  orders: null,
  auto: null,
  autoHistory: null,
  dataStatus: null,
  apiLimits: null,
  suggestions: null,
  watchlist: null,
  movers: null,
  wash: null,
  pdt: null,
  tax: null,
};

function isLocalhostAccess() {
  const hostname = window.location.hostname;
  return hostname === "localhost" || hostname === "127.0.0.1" || hostname === "::1";
}

function normalizeTab(value) {
  const tab = String(value || "").replace(/^#\/?/, "");
  return tabIds.has(tab) ? tab : DEFAULT_TAB;
}

function storedDashboardTab() {
  try {
    return normalizeTab(window.localStorage.getItem(DASHBOARD_TAB_STORAGE_KEY));
  } catch {
    return DEFAULT_TAB;
  }
}

function initialDashboardTab() {
  if (typeof window === "undefined") return DEFAULT_TAB;
  const hashTab = String(window.location.hash || "").replace(/^#\/?/, "");
  return tabIds.has(hashTab) ? hashTab : storedDashboardTab();
}

function persistDashboardTab(tab) {
  const next = normalizeTab(tab);
  try {
    window.localStorage.setItem(DASHBOARD_TAB_STORAGE_KEY, next);
  } catch {
    // Ignore private-mode storage failures; the URL hash still preserves refreshes.
  }
  if (window.location.hash.replace(/^#\/?/, "") !== next) {
    window.history.replaceState(null, "", `#${next}`);
  }
}

function storedDashboardAccount() {
  try {
    return window.localStorage.getItem(DASHBOARD_ACCOUNT_STORAGE_KEY) || "";
  } catch {
    return "";
  }
}

function persistDashboardAccount(selector) {
  try {
    if (selector) {
      window.localStorage.setItem(DASHBOARD_ACCOUNT_STORAGE_KEY, selector);
    } else {
      window.localStorage.removeItem(DASHBOARD_ACCOUNT_STORAGE_KEY);
    }
  } catch {
    // Ignore private-mode storage failures.
  }
}

function sleep(ms) {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

function runQueuedApi(task) {
  return new Promise((resolve, reject) => {
    const run = () => {
      apiActiveRequests += 1;
      Promise.resolve()
        .then(task)
        .then(resolve, reject)
        .finally(() => {
          apiActiveRequests = Math.max(0, apiActiveRequests - 1);
          const next = apiQueue.shift();
          if (next) next();
        });
    };
    if (apiActiveRequests < API_CLIENT_CONCURRENCY) {
      run();
    } else {
      apiQueue.push(run);
    }
  });
}

async function fetchApiJson(path, options = {}) {
  const controller = new AbortController();
  const timeoutMs = options.timeoutMs || 60000;
  const timer = window.setTimeout(() => controller.abort(), timeoutMs);
  try {
    const headers = {
      accept: "application/json",
      "x-mlai-client-timezone": CLIENT_TIME_ZONE,
      ...(options.headers || {}),
    };
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
      const error = new Error(message);
      error.status = res.status;
      error.apiPayload = json;
      error.retryAfterSeconds = Number(res.headers.get("retry-after") || json.retry_after_seconds || 0);
      throw error;
    }
    return json;
  } finally {
    window.clearTimeout(timer);
  }
}

async function api(path, options = {}) {
  return runQueuedApi(async () => {
    let lastError;
    for (let attempt = 0; attempt <= API_MAX_RETRIES; attempt += 1) {
      try {
        return await fetchApiJson(path, options);
      } catch (err) {
        lastError = err;
        if (err?.status !== 429 || attempt >= API_MAX_RETRIES) break;
        const retryAfter = Number.isFinite(err.retryAfterSeconds) && err.retryAfterSeconds > 0 ? err.retryAfterSeconds : 1;
        await sleep(Math.min(5000, retryAfter * 1000));
      }
    }
    throw lastError;
  });
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

function realtimeLabel(payload) {
  const transport = text(payload?.transport, "");
  if (transport.includes("http3")) return "Realtime H3 stream";
  if (transport.includes("tcp")) return "Realtime HTTPS stream";
  return "Snapshot polling";
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

function parseClientDate(value) {
  if (!value) return null;
  if (value instanceof Date) return Number.isNaN(value.getTime()) ? null : value;
  const raw = String(value);
  const date = /^\d{4}-\d{2}-\d{2}$/.test(raw) ? new Date(`${raw}T00:00:00`) : new Date(raw);
  return date && !Number.isNaN(date.getTime()) ? date : null;
}

function clientDateKey(value) {
  const raw = text(value, "");
  if (/^\d{4}-\d{2}-\d{2}$/.test(raw)) return raw;
  const date = parseClientDate(raw);
  return date ? dateInputValue(date) : "";
}

function formatClientDate(value, options, fallback = "not available") {
  const date = parseClientDate(value);
  if (!date) return fallback;
  return new Intl.DateTimeFormat(CLIENT_LOCALE, { timeZone: CLIENT_TIME_ZONE, ...options }).format(date);
}

function dateText(value) {
  return formatClientDate(value, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function dateOnlyText(value) {
  return formatClientDate(value, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  });
}

function clientTimeText(value = new Date()) {
  return formatClientDate(value, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function sortableDateValue(value) {
  return parseClientDate(value)?.getTime() || 0;
}

function dateInputValue(value = new Date()) {
  const date = value instanceof Date ? value : parseClientDate(value);
  if (!date || Number.isNaN(date.getTime())) return "";
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
  return compact
    ? formatClientDate(value, { month: "short", day: "numeric" }, "")
    : formatClientDate(value, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }, "");
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
  let timeframe = "5Min";
  let limit = 1000;
  if (days > 1 && days <= 3) {
    timeframe = "15Min";
  } else if (days > 3 && days <= 7) {
    timeframe = "30Min";
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

function marketBarsBatchSizeFor(apiLimits, chartSpec) {
  const limits = dataOf(apiLimits).limits || dataOf(apiLimits);
  const maxSymbols = Math.max(1, number(limits.market_bars_max_symbols, 50));
  const maxTotalBars = Math.max(1, number(limits.market_bars_max_total_bars, 25000));
  const maxByBars = Math.max(1, Math.floor(maxTotalBars / Math.max(1, chartSpec?.limit || 1000)));
  return Math.max(1, Math.min(MARKET_BARS_FALLBACK_BATCH_SIZE, maxSymbols, maxByBars));
}

function dashboardLimitsFor(apiLimits) {
  const limits = dataOf(apiLimits).limits || dataOf(apiLimits);
  return {
    ordersLimit: Math.max(1, number(limits.dashboard_orders_limit, DASHBOARD_ORDERS_FALLBACK_LIMIT)),
    tableInitialRows: Math.max(
      1,
      number(limits.dashboard_table_initial_rows, DASHBOARD_TABLE_FALLBACK_INITIAL_ROWS)
    ),
    tablePageRows: Math.max(
      1,
      number(limits.dashboard_table_page_rows, DASHBOARD_TABLE_FALLBACK_PAGE_ROWS)
    ),
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

function accountSelectorForAutoEntry(entry) {
  return accountSelector({
    provider: entry?.provider,
    account_ref: entry?.account_ref,
    name: entry?.account_ref,
    account_id: entry?.account_ref,
  });
}

function filterRowsByAccount(rows, selector) {
  if (!selector) return rows;
  return rows.filter((row) => row.selector === selector || row.account_selector === selector || accountSelector(row.account) === selector);
}

function filterAutoByAccount(payload, selector) {
  if (!selector || !payload) return payload;
  const data = dataOf(payload);
  const accounts = autoAccounts(payload).filter((entry) => accountSelectorForAutoEntry(entry) === selector);
  if (payload.data !== undefined) {
    return { ...payload, data: { ...data, accounts } };
  }
  return { ...data, accounts };
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
      equity: firstDefined(entry.equity, entry.portfolio_value, account.equity, account.portfolio_value),
      cash: firstDefined(entry.cash, account.cash),
      buying_power: firstDefined(entry.buying_power, account.buying_power),
      day_pnl: firstDefined(entry.day_pnl, account.day_pnl),
      day_pnl_pct: firstDefined(entry.day_pnl_pct, account.day_pnl_pct),
      pdt: firstDefined(entry.pattern_day_trader, account.pattern_day_trader),
      trading_blocked: firstDefined(entry.trading_blocked, account.trading_blocked),
      broker_status: entry.broker_status || entry.status || account.broker_status || account.status,
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

function positionEntryDate(row) {
  const raw = firstDefined(row.entry_timestamp, row.entry_time, row.entry_at, row.opened_at, row.buy_timestamp, row.entry_date);
  return parseClientDate(raw);
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
  const basis = number(positionCostBasis(row));
  const pnl = number(positionPnl(row), NaN);
  if (basis && Number.isFinite(pnl)) return (pnl / Math.abs(basis)) * 100;
  if (row.unrealized_pnl_pct !== undefined) return number(row.unrealized_pnl_pct);
  if (row.pnl_percent !== undefined) return number(row.pnl_percent);
  if (row.pnl_pct !== undefined) return number(row.pnl_pct);
  if (row.unrealized_plpc !== undefined) return normalizePct(row.unrealized_plpc);
  return 0;
}

function positionsUnrealizedPnl(rows) {
  return arrayFrom(rows).reduce((sum, row) => sum + number(positionPnl(row)), 0);
}

function positionKey(row) {
  const selector = row.account_selector || accountSelector(row.account);
  const symbol = text(row.symbol, "").toUpperCase();
  return selector && symbol ? `${selector}:${symbol}` : "";
}

function providerPositionIndex(rows) {
  const index = new Map();
  arrayFrom(rows).forEach((row) => {
    const key = positionKey(row);
    if (key) index.set(key, row);
  });
  return index;
}

function liveCostBasis(row) {
  if (row.cost_basis !== undefined) return row.cost_basis;
  const cost = number(row.avg_entry_price ?? row.avg_cost, NaN);
  const qty = number(row.qty ?? row.quantity ?? row.shares, NaN);
  return Number.isFinite(cost) && Number.isFinite(qty) ? cost * qty : undefined;
}

function mergePositionMarketData(row, providerIndex) {
  const live = providerIndex.get(positionKey(row));
  if (!live) return row;
  const costBasis = liveCostBasis(live);
  return {
    ...row,
    qty: live.qty ?? live.quantity ?? row.qty,
    quantity: live.quantity ?? live.qty ?? row.quantity,
    shares: live.qty ?? live.quantity ?? row.shares,
    avg_entry_price: live.avg_entry_price ?? live.avg_cost ?? row.avg_entry_price,
    current_price: live.current_price ?? live.now ?? live.market_price ?? row.current_price,
    market_value: live.market_value ?? row.market_value,
    cost_basis: costBasis ?? row.cost_basis,
    unrealized_pl: live.unrealized_pl ?? live.unrealized_pnl ?? row.unrealized_pl,
    unrealized_pnl: live.unrealized_pl ?? live.unrealized_pnl ?? row.unrealized_pnl,
    unrealized_plpc: live.unrealized_plpc ?? row.unrealized_plpc,
    unrealized_pnl_pct: live.unrealized_plpc ?? row.unrealized_pnl_pct,
    provider_live_source: live.source || row.provider_live_source,
  };
}

function mergeRowsWithProviderMarketData(rows, providerPositions) {
  const index = providerPositionIndex(providerPositions);
  return arrayFrom(rows).map((row) => mergePositionMarketData(row, index));
}

function autoOpenPnlFromProvider(auto, providerPositions) {
  const managed = autoManagedRows(auto);
  const providerRows = arrayFrom(providerPositions);
  const index = providerPositionIndex(providerRows);
  if (providerRows.length) {
    return managed.reduce((sum, row) => {
      if (!index.has(positionKey(row))) return sum;
      return sum + number(positionPnl(mergePositionMarketData(row, index)));
    }, 0);
  }
  return positionsUnrealizedPnl(managed);
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

function orderRealizedPnl(row) {
  return firstDefined(row.realized_pnl, row.pnl, row.closed_pnl, row.realized_gain_loss);
}

function orderPnlPct(row) {
  return firstDefined(row.realized_pnl_pct, row.pnl_pct, row.realized_gain_loss_pct);
}

function orderPnlText(row) {
  if (text(row.side, "").toLowerCase() !== "sell") return "-";
  const pnl = orderRealizedPnl(row);
  if (pnl === undefined || pnl === null || pnl === "") return "pending sync";
  const pctValue = orderPnlPct(row);
  return pctValue === undefined || pctValue === null || pctValue === ""
    ? money(pnl)
    : `${money(pnl)} (${pct(pctValue)})`;
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

function extractMovers(payload) {
  const data = dataOf(payload);
  const gainers = arrayFrom(data.gainers).map((row) => ({ ...row, direction: "gainer" }));
  const losers = arrayFrom(data.losers).map((row) => ({ ...row, direction: "loser" }));
  const flat = arrayFrom(data.movers || data.results || data.rows);
  return flat.length ? flat : [...gainers, ...losers];
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

function barsMetaFromPayload(payload) {
  const data = dataOf(payload);
  const bars = barsFromPayload(payload);
  return {
    source: text(data.source, "not loaded"),
    cacheRowsStored: number(data.cache_rows_stored, 0),
    bars: bars.length,
  };
}

function barDate(row) {
  const raw = firstDefined(row.t, row.timestamp, row.datetime, row.date, row.time);
  return parseClientDate(raw);
}

function barClose(row) {
  return number(firstDefined(row.close, row.c, row.price), NaN);
}

function barCacheKey(symbol, chartSpec) {
  return `${text(symbol, "").toUpperCase()}:${chartSpec?.cacheKey || "default"}`;
}

function hasBarPayload(row, barsBySymbol = {}, chartSpec) {
  const symbol = text(row.symbol, "").toUpperCase();
  if (!symbol) return false;
  return Object.prototype.hasOwnProperty.call(barsBySymbol, barCacheKey(symbol, chartSpec));
}

function chartLoadingForRows(rows, barsBySymbol = {}, chartSpec, loadingKeys) {
  const safeRows = Array.isArray(rows) ? rows : [];
  if (!safeRows.length) return false;
  return safeRows.some((row) => {
    const symbol = text(row.symbol, "").toUpperCase();
    if (!symbol) return false;
    const key = barCacheKey(symbol, chartSpec);
    return Boolean(loadingKeys?.has(key)) || !hasBarPayload(row, barsBySymbol, chartSpec);
  });
}

function chunkArray(values, size) {
  const chunks = [];
  for (let index = 0; index < values.length; index += size) {
    chunks.push(values.slice(index, index + size));
  }
  return chunks;
}

const featureMeanings = {
  atr_14: "Average true range over 14 sessions. Higher values mean the symbol is moving more each day.",
  bb_position: "Where the current price sits inside its Bollinger Band. High values are near the upper band; low values are near the lower band.",
  close_to_high_20d: "How close the latest close is to the 20-day high. This captures whether the symbol is trading near recent strength.",
  close_to_low_20d: "How far the latest close is from the 20-day low. This captures whether buyers have moved price away from recent weakness.",
  macd: "Momentum difference between fast and slow moving averages.",
  macd_hist: "MACD histogram. Positive values usually mean momentum is improving; negative values mean it is fading.",
  macd_signal: "Smoothed MACD signal line used to judge momentum trend changes.",
  obv_slope_20d: "Twenty-day on-balance-volume trend. Positive values mean volume has tended to support upward price movement.",
  rank_momentum: "Cross-symbol momentum rank. It shows how this symbol's momentum compares with the current trading universe.",
  rank_volatility: "Cross-symbol volatility rank. It shows whether this symbol is calmer or more volatile than peers.",
  rank_volume_ratio: "Cross-symbol unusual-volume rank. Higher values mean volume is elevated versus other symbols.",
  relative_qqq_20d: "Twenty-day performance compared with QQQ. Negative values mean it lagged QQQ over that window.",
  relative_return_20d: "Twenty-day performance compared with the local benchmark universe. Negative values mean it lagged peers.",
  relative_sector_avg_20d: "Twenty-day performance compared with its sector average. This often anchors symbols that lag their sector.",
  relative_spy_20d: "Twenty-day performance compared with SPY. Negative values mean it lagged the broad market.",
  return_5d: "The symbol's five-day return.",
  return_20d: "The symbol's twenty-day return.",
  return_60d: "The symbol's sixty-day return.",
  rsi_14: "Fourteen-day relative strength index. High values can mean strong momentum or an overbought move.",
  sma_cross_50_200: "Relationship between the 50-day and 200-day moving averages. Positive values favor longer-term uptrends.",
  volatility_20d: "Twenty-day realized volatility. Higher values mean more recent price instability.",
  volume_ratio_20d: "Latest volume compared with the 20-day average. Values above 1 mean above-normal trading activity.",
};

function featureMeaning(feature) {
  const key = text(feature, "").toLowerCase();
  if (!key) return "Model feature used in the latest prediction.";
  if (featureMeanings[key]) return featureMeanings[key];
  const returnMatch = key.match(/^return_(\d+)d$/);
  if (returnMatch) return `The symbol's ${returnMatch[1]}-day return.`;
  const benchmarkMatch = key.match(/^(sp500|spy|qqq|vix)_return_(\d+)d$/);
  if (benchmarkMatch) {
    const label = benchmarkMatch[1].toUpperCase();
    return `${label} ${benchmarkMatch[2]}-day return used as market context for this symbol.`;
  }
  const feedMatch = key.match(/^feed_(.+)$/);
  if (feedMatch) {
    return `Feed-derived signal from news, filings, relationships, or sentiment. It helps the model account for current external context.`;
  }
  const relativeMatch = key.match(/^relative_(.+)$/);
  if (relativeMatch) {
    return "Relative performance feature. It compares this symbol with a benchmark, sector, ETF, or peer group.";
  }
  const rankMatch = key.match(/^rank_(.+)$/);
  if (rankMatch) {
    return "Cross-sectional rank versus other symbols in the current universe. It tells the model how this symbol compares today.";
  }
  if (key.includes("sentiment")) return "News or filing sentiment score. Positive values are generally supportive; negative values are generally a drag.";
  if (key.includes("correlation")) return "Relationship/correlation feature that captures how this symbol moves with related companies or themes.";
  if (key.includes("volume")) return "Volume feature. It captures whether trading activity is normal, elevated, or fading.";
  if (key.includes("volatility")) return "Volatility feature. It captures recent instability and risk.";
  if (key.includes("momentum")) return "Momentum feature. It captures recent price strength or weakness.";
  return "Model feature used in the latest prediction. The impact column shows whether it helped or hurt this symbol's score.";
}

function sentimentTone(score) {
  const n = number(score, 0);
  if (n >= 0.1) return "Positive";
  if (n <= -0.1) return "Negative";
  return "Neutral";
}

function sentimentToneClass(score) {
  const n = number(score, 0);
  if (n >= 0.1) return "gain";
  if (n <= -0.1) return "loss";
  return "";
}

function explainFeatures(payload) {
  const rows = arrayFrom(dataOf(payload).features);
  return rows
    .map((row) => ({
      ...row,
      feature: text(row.feature, "-"),
      feature_value: number(row.feature_value, 0),
      shap_value: number(row.shap_value, 0),
    }))
    .filter((row) => row.feature !== "-");
}

function topExplainFeatures(payload, direction) {
  const rows = explainFeatures(payload);
  const filtered =
    direction === "positive"
      ? rows.filter((row) => row.shap_value > 0).sort((a, b) => b.shap_value - a.shap_value)
      : rows.filter((row) => row.shap_value < 0).sort((a, b) => a.shap_value - b.shap_value);
  return filtered.slice(0, direction === "positive" ? 8 : 10);
}

function positionPnlSeries(row, barsBySymbol = {}, chartSpec) {
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
  return values.length >= 2 ? values : [];
}

function positionPnlBarSeries(row, barsBySymbol = {}, chartSpec) {
  const symbol = text(row.symbol, "").toUpperCase();
  const bars = barsFromPayload(barsBySymbol[barCacheKey(symbol, chartSpec)]);
  const qty = positionQty(row);
  const cost = number(positionCost(row), NaN);
  return bars
    .map((bar) => ({ bar, date: barDate(bar) }))
    .filter(({ date }) => inChartRange(date, chartSpec))
    .sort((a, b) => a.date - b.date)
    .map(({ bar, date }) => {
      const close = barClose(bar);
      return Number.isFinite(close) && Number.isFinite(cost) ? { value: (close - cost) * qty, date } : null;
    })
    .filter(Boolean);
}

function aggregatePositionPnlSeries(rows, barsBySymbol = {}, chartSpec, fallbackValue = 0) {
  const series = rows
    .map((row) => positionPnlBarSeries(row, barsBySymbol, chartSpec))
    .filter((values) => values.length >= 2);
  if (!series.length) return [];
  const timestamps = Array.from(
    new Set(series.flatMap((values) => values.map((point) => point.date.getTime())))
  ).sort((a, b) => a - b);
  const cursors = series.map(() => 0);
  return timestamps.map((timestamp) => {
    const value = series.reduce((sum, values, seriesIndex) => {
      while (
        cursors[seriesIndex] + 1 < values.length &&
        values[cursors[seriesIndex] + 1].date.getTime() <= timestamp
      ) {
        cursors[seriesIndex] += 1;
      }
      return sum + number(values[cursors[seriesIndex]]?.value);
    }, 0);
    return { value, date: new Date(timestamp) };
  });
}

function accountPnlSeries(account, positions, barsBySymbol, chartSpec) {
  const selector = account.selector || accountSelector(account.account || account);
  const accountPositions = positions.filter((row) => row.account_selector === selector);
  return aggregatePositionPnlSeries(accountPositions, barsBySymbol, chartSpec);
}

function taxDetailRows(payload) {
  return arrayFrom(dataOf(payload).details)
    .slice()
    .sort((a, b) => {
      const byExit = sortableDateValue(b.exit_date) - sortableDateValue(a.exit_date);
      if (byExit) return byExit;
      const byEntry = sortableDateValue(b.entry_date) - sortableDateValue(a.entry_date);
      if (byEntry) return byEntry;
      return [
        text(a.provider, ""),
        text(a.account_ref, ""),
        text(a.symbol, ""),
      ]
        .join(":")
        .localeCompare(
          [text(b.provider, ""), text(b.account_ref, ""), text(b.symbol, "")].join(":")
        );
    });
}

function taxQuarterRows(payload) {
  return arrayFrom(dataOf(payload).by_quarter);
}

function washGroupKey(row) {
  const sellDate = clientDateKey(firstDefined(row.sell_date, row.sell_timestamp_utc, row.sold_at, row.sold_date, row.date));
  const windowEnd = clientDateKey(firstDefined(row.wash_window_end, row.window_end, row.window_end_date, row.expires_at, row.expiration_date));
  return [
    text(firstDefined(row.tax_universe, row.universe, row.account_mode), "unknown"),
    text(row.symbol, "symbol").toUpperCase(),
    text(sellDate, "date"),
    text(windowEnd, "window"),
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
    existing.sell_date = clientDateKey(firstDefined(row.sell_date, row.sell_timestamp_utc, row.sold_at, row.sold_date, row.date));
    existing.wash_window_end = clientDateKey(firstDefined(row.wash_window_end, row.window_end, row.window_end_date, row.expires_at, row.expiration_date));
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

function PagedDataTable({
  rows,
  columns,
  empty,
  initial = DASHBOARD_TABLE_FALLBACK_INITIAL_ROWS,
  step = DASHBOARD_TABLE_FALLBACK_PAGE_ROWS,
}) {
  const initialRows = Math.max(1, number(initial, DASHBOARD_TABLE_FALLBACK_INITIAL_ROWS));
  const stepRows = Math.max(1, number(step, DASHBOARD_TABLE_FALLBACK_PAGE_ROWS));
  const [limit, setLimit] = useState(initialRows);
  const safeRows = Array.isArray(rows) ? rows : [];
  useEffect(() => {
    setLimit(initialRows);
  }, [safeRows.length, initialRows]);
  return (
    <div className="paged-table">
      <DataTable rows={safeRows.slice(0, limit)} columns={columns} empty={empty} />
      {safeRows.length > limit && (
        <button className="secondary" onClick={() => setLimit((current) => current + stepRows)}>
          Show more +{stepRows}
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

function PnlChart({
  values,
  entryDate = null,
  height = 260,
  compact = false,
  emptyLabel = "No P&L series",
  loading = false,
}) {
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
              ? { value: number(point.value, NaN), date: parseClientDate(point.date) }
              : { value: number(point, NaN), date: null }
          )
          .filter((point) => Number.isFinite(point.value))
      : [];
    if (series.length < 2) {
      pointsRef.current = [];
      setHover(null);
      if (!loading) {
        ctx.fillStyle = "#657287";
        ctx.font = `${(compact ? 11 : 13) * ratio}px system-ui`;
        ctx.textAlign = "center";
        ctx.fillText(emptyLabel, width / 2, realHeight / 2);
      }
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
    const validDates = series.map((point) => point.date).filter((date) => date && !Number.isNaN(date.getTime()));
    const firstDate = validDates.length ? validDates[0] : null;
    const lastDate = validDates.length ? validDates[validDates.length - 1] : null;
    const timeSpan = firstDate && lastDate ? Math.max(1, lastDate.getTime() - firstDate.getTime()) : 1;
    const xFor = (point, index) => {
      if (point.date && firstDate && lastDate) {
        return padX + ((point.date.getTime() - firstDate.getTime()) / timeSpan) * chartWidth;
      }
      return padX + (index / (series.length - 1)) * chartWidth;
    };
    const points = series.map((point, index) => [
      xFor(point, index),
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

    ctx.save();
    ctx.strokeStyle = "#334155";
    ctx.lineWidth = (compact ? 1.2 : 1.6) * ratio;
    ctx.setLineDash([5 * ratio, 4 * ratio]);
    ctx.beginPath();
    ctx.moveTo(padX, zeroY);
    ctx.lineTo(width - padX, zeroY);
    ctx.stroke();
    ctx.restore();

    const entryLabel = compact ? "Entry" : "Entry price / $0 P&L";
    ctx.save();
    ctx.font = `${(compact ? 9 : 11) * ratio}px system-ui`;
    ctx.textBaseline = "middle";
    ctx.textAlign = "left";
    const labelX = padX + 6 * ratio;
    const labelY = Math.min(Math.max(zeroY - 11 * ratio, padTop + 8 * ratio), realHeight - padBottom - 8 * ratio);
    const labelWidth = ctx.measureText(entryLabel).width + 10 * ratio;
    ctx.fillStyle = "rgba(255, 255, 255, 0.82)";
    ctx.fillRect(labelX - 5 * ratio, labelY - 9 * ratio, labelWidth, 18 * ratio);
    ctx.fillStyle = "#334155";
    ctx.fillText(entryLabel, labelX, labelY);
    ctx.restore();

    const markerDate = parseClientDate(entryDate);
    if (
      markerDate &&
      firstDate &&
      lastDate &&
      !Number.isNaN(markerDate.getTime()) &&
      markerDate >= firstDate &&
      markerDate <= lastDate
    ) {
      const entryX = padX + ((markerDate.getTime() - firstDate.getTime()) / timeSpan) * chartWidth;
      ctx.save();
      ctx.strokeStyle = "rgba(27, 94, 236, 0.75)";
      ctx.lineWidth = (compact ? 1.2 : 1.5) * ratio;
      ctx.setLineDash([3 * ratio, 4 * ratio]);
      ctx.beginPath();
      ctx.moveTo(entryX, padTop);
      ctx.lineTo(entryX, realHeight - padBottom);
      ctx.stroke();
      ctx.font = `${(compact ? 8 : 10) * ratio}px system-ui`;
      ctx.textBaseline = "top";
      ctx.textAlign = entryX > width - padX - 52 * ratio ? "right" : "left";
      ctx.fillStyle = "#1b5eec";
      ctx.fillText("Buy", entryX + (ctx.textAlign === "right" ? -4 : 4) * ratio, padTop + 2 * ratio);
      ctx.restore();
    }

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
  }, [values, entryDate, height, compact, emptyLabel, loading]);

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
      {loading && (
        <div className="chart-loading" aria-live="polite">
          <span className="spinner" aria-hidden="true" />
          <span>Loading bars</span>
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

function Sidebar({ activeTab, setActiveTab }) {
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

function AccountFilter({ accounts, selectedAccount, setSelectedAccount }) {
  return (
    <label className="account-filter">
      <span>Account</span>
      <select value={selectedAccount} onChange={(event) => setSelectedAccount(event.target.value)}>
        <option value="">All accounts</option>
        {accounts.map((account) => (
          <option key={account.selector} value={account.selector}>
            {account.selector}
          </option>
        ))}
      </select>
    </label>
  );
}

function SymbolButton({ symbol, onSymbolClick }) {
  const safeSymbol = text(symbol, "-").toUpperCase();
  if (!onSymbolClick || safeSymbol === "-") return safeSymbol;
  return (
    <button type="button" className="symbol-link" onClick={() => onSymbolClick(safeSymbol)}>
      {safeSymbol}
    </button>
  );
}

function PositionTable({ rows, empty, mlqLookup, paged = false, tableLimits, onSymbolClick }) {
  const Table = paged ? PagedDataTable : DataTable;
  return (
    <Table
      rows={rows}
      empty={empty}
      initial={tableLimits?.tableInitialRows}
      step={tableLimits?.tablePageRows}
      columns={[
        { label: "Symbol", value: (r) => <SymbolButton symbol={r.symbol} onSymbolClick={onSymbolClick} /> },
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

function OrderTable({ rows, paged = false, tableLimits }) {
  const Table = paged ? PagedDataTable : DataTable;
  return (
    <Table
      rows={rows}
      initial={tableLimits?.tableInitialRows}
      step={tableLimits?.tablePageRows}
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
        { label: "P&L", value: orderPnlText, className: (r) => tone(orderRealizedPnl(r)) },
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

function Overview({ accounts, positions, orders, auto, autoHistory, mlqLookup, chartSpec, barsBySymbol, barLoadingKeys }) {
  const managed = autoManagedRows(auto);
  const autoAccountsRows = autoAccounts(auto);
  const equity = accounts.reduce((sum, row) => sum + number(row.equity), 0);
  const cash = accounts.reduce((sum, row) => sum + number(row.cash), 0);
  const buyingPower = accounts.reduce((sum, row) => sum + number(row.buying_power), 0);
  const openValue = positions.reduce((sum, row) => sum + number(positionMarketValue(row)), 0);
  const unrealized = positionsUnrealizedPnl(positions);
  const autoOpenPnl = autoOpenPnlFromProvider(auto, positions);
  const closedPnl = autoAccountsRows.reduce((sum, row) => sum + number(row.closed_pnl), 0);
  const perfValues = aggregatePositionPnlSeries(positions, barsBySymbol, chartSpec, unrealized);
  const perfLoading = chartLoadingForRows(positions, barsBySymbol, chartSpec, barLoadingKeys);
  const perfTrades = tradeHistoryRows(autoHistory).length;
  const allocation = allocationRows(positions);

  return (
    <div className="dashboard-grid">
      <article className="surface balance-panel">
        <div className="section-head">
          <div>
            <span className="eyebrow">Real trading performance</span>
            <h2>Provider open P&L</h2>
          </div>
          <strong className={tone(unrealized)}>{money(unrealized)}</strong>
        </div>
        <PnlChart values={perfValues} loading={perfLoading} emptyLabel="No bars for selected range" />
        <p className="chart-note">
          {chartSpec.label}: {positions.length} provider positions from {chartSpec.timeframe} bars. Auto closed P&L: {money(closedPnl)} across {perfTrades} trades.
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
          <strong className={tone(autoOpenPnl + closedPnl)}>{money(autoOpenPnl + closedPnl)}</strong>
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

function AccountPerformanceCards({ accounts, positions, barsBySymbol, chartSpec, barLoadingKeys }) {
  return (
    <div className="account-card-grid">
      {accounts.map((account) => {
        const accountPositions = positions.filter((row) => row.account_selector === account.selector);
        const allocation = allocationRows(accountPositions);
        const pnlSeries = accountPnlSeries(account, positions, barsBySymbol, chartSpec);
        const loading = chartLoadingForRows(accountPositions, barsBySymbol, chartSpec, barLoadingKeys);
        const current = positionsUnrealizedPnl(accountPositions);
        return (
          <article className="surface account-performance-card" key={account.selector}>
            <div className="section-head compact">
              <div>
                <span className="eyebrow">{account.provider}</span>
                <h2>{account.selector}</h2>
              </div>
              <strong className={tone(current)}>{money(current)}</strong>
            </div>
            <PnlChart values={pnlSeries} height={150} compact loading={loading} emptyLabel="No bars" />
            <AllocationBars rows={allocation} empty="No open positions." />
          </article>
        );
      })}
    </div>
  );
}

function AccountsView({ rows, positions, barsBySymbol, chartSpec, barLoadingKeys }) {
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
      <AccountPerformanceCards
        accounts={rows}
        positions={positions}
        barsBySymbol={barsBySymbol}
        chartSpec={chartSpec}
        barLoadingKeys={barLoadingKeys}
      />
    </div>
  );
}

function PositionChartGrid({
  rows,
  barsBySymbol,
  mlqLookup,
  chartSpec,
  tableLimits,
  barLoadingKeys,
  onSymbolClick,
}) {
  const initialRows = Math.max(
    1,
    number(tableLimits?.tableInitialRows, DASHBOARD_TABLE_FALLBACK_INITIAL_ROWS)
  );
  const stepRows = Math.max(1, number(tableLimits?.tablePageRows, DASHBOARD_TABLE_FALLBACK_PAGE_ROWS));
  const [limit, setLimit] = useState(initialRows);
  const safeRows = Array.isArray(rows) ? rows : [];
  useEffect(() => setLimit(initialRows), [safeRows.length, initialRows]);
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
          const key = barCacheKey(row.symbol, chartSpec);
          const loading = Boolean(barLoadingKeys?.has(key)) || !hasBarPayload(row, barsBySymbol, chartSpec);
          const barsMeta = barsMetaFromPayload(barsBySymbol[key]);
          const current = number(positionPnl(row));
          return (
            <article className="position-card" key={`${row.account_selector}:${row.symbol}`}>
              <div className="position-card-head">
                <div>
                  <strong>
                    <SymbolButton symbol={row.symbol} onSymbolClick={onSymbolClick} />
                  </strong>
                  <span>{row.account_selector}</span>
                </div>
                <b className={tone(current)}>{money(current)}</b>
              </div>
              <PnlChart
                values={values}
                entryDate={positionEntryDate(row)}
                height={130}
                compact
                emptyLabel="No bars for selected range"
                loading={loading}
              />
              <div className="position-card-stats">
                <span>Qty {positionQty(row).toFixed(2)}</span>
                <span>MLQ {positionMlq(row, mlqLookup)}</span>
                <span title={`stored ${barsMeta.cacheRowsStored} new rows`}>Bars {barsMeta.source} ({barsMeta.bars})</span>
                <span className={tone(positionPnlPct(row))}>{pct(positionPnlPct(row))}</span>
              </div>
            </article>
          );
        })}
      </div>
      {safeRows.length > limit && (
        <button className="secondary" onClick={() => setLimit((current) => current + stepRows)}>
          Show more +{stepRows}
        </button>
      )}
    </article>
  );
}

function PositionsView({
  positions,
  auto,
  mlqLookup,
  barsBySymbol,
  chartSpec,
  tableLimits,
  barLoadingKeys,
  onSymbolClick,
}) {
  const managed = mergeRowsWithProviderMarketData(autoManagedRows(auto), positions);
  const unmanaged = mergeRowsWithProviderMarketData(autoUnmanagedRows(auto), positions);
  return (
    <div className="section-layout">
      <PositionChartGrid
        rows={positions}
        barsBySymbol={barsBySymbol}
        mlqLookup={mlqLookup}
        chartSpec={chartSpec}
        tableLimits={tableLimits}
        barLoadingKeys={barLoadingKeys}
        onSymbolClick={onSymbolClick}
      />
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Provider query</span>
            <h2>All Open Positions</h2>
          </div>
          <span className="status-pill">{positions.length}</span>
        </div>
        <PositionTable
          rows={positions}
          mlqLookup={mlqLookup}
          paged
          tableLimits={tableLimits}
          onSymbolClick={onSymbolClick}
        />
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Auto rules</span>
            <h2>Tracked vs Not Tracked</h2>
          </div>
          <span className="status-pill">{managed.length} tracked / {unmanaged.length} not tracked</span>
        </div>
        <PositionTable
          rows={[...managed, ...unmanaged]}
          mlqLookup={mlqLookup}
          paged
          tableLimits={tableLimits}
          onSymbolClick={onSymbolClick}
        />
      </article>
    </div>
  );
}

function SentimentSummary({ payload }) {
  const data = dataOf(payload);
  const sources = arrayFrom(data.by_source);
  const recent = arrayFrom(data.recent).slice(0, 10);
  const sentiment7d = number(data.sentiment_7d, 0);
  const sentiment30d = number(data.sentiment_30d, 0);
  return (
    <article className="insight-section">
      <div className="section-head compact">
        <div>
          <span className="eyebrow">Feeds</span>
          <h2>Sentiment</h2>
        </div>
        <strong className={sentimentToneClass(sentiment7d)}>{sentimentTone(sentiment7d)}</strong>
      </div>
      <div className="insight-metrics">
        <InfoTile label="7-day" value={sentiment7d.toFixed(3)} detail={`${number(data.articles_7d)} articles`} valueTone={sentimentToneClass(sentiment7d)} />
        <InfoTile label="30-day" value={sentiment30d.toFixed(3)} detail={`${number(data.articles_30d)} articles`} valueTone={sentimentToneClass(sentiment30d)} />
        <InfoTile label="SEC 8-K" value={number(data.sec_8k_count)} detail="recent filings" />
        <InfoTile label="Form 4" value={number(data.sec_form4_count)} detail="insider filings" />
      </div>
      <div className="source-list">
        {sources.map((row) => (
          <span key={row.source}>
            {text(row.source, "source")}: <b>{number(row.count)}</b>
          </span>
        ))}
      </div>
      <DataTable
        rows={recent}
        empty="No recent headlines."
        columns={[
          { label: "Published", value: (r) => dateText(r.published_at) },
          { label: "Source", value: (r) => text(r.source, "-") },
          { label: "Sentiment", value: (r) => number(r.sentiment).toFixed(2), className: (r) => sentimentToneClass(r.sentiment) },
          { label: "Headline", value: (r) => text(r.title, "-") },
        ]}
      />
    </article>
  );
}

function ExplainTable({ rows, empty }) {
  return (
    <DataTable
      rows={rows}
      empty={empty}
      columns={[
        { label: "Feature", value: (r) => text(r.feature, "-") },
        { label: "Impact", value: (r) => number(r.shap_value).toFixed(4), className: (r) => tone(r.shap_value) },
        { label: "Value", value: (r) => number(r.feature_value).toFixed(4) },
        { label: "Meaning", value: (r) => featureMeaning(r.feature) },
      ]}
    />
  );
}

function ExplainSummary({ payload }) {
  const data = dataOf(payload);
  const positive = topExplainFeatures(payload, "positive");
  const negative = topExplainFeatures(payload, "negative");
  const predicted = number(data.predicted, 0);
  const baseValue = number(data.base_value, 0);
  return (
    <article className="insight-section">
      <div className="section-head compact">
        <div>
          <span className="eyebrow">ML explain</span>
          <h2>{text(data.symbol, "Symbol")} model drivers</h2>
        </div>
        <strong className={tone(predicted - baseValue)}>{number(predicted).toFixed(4)}</strong>
      </div>
      <div className="insight-metrics">
        <InfoTile label="Date" value={text(data.date, "not available")} detail="feature snapshot" />
        <InfoTile label="Base" value={baseValue.toFixed(4)} detail="average model value" />
        <InfoTile label="Predicted" value={predicted.toFixed(4)} detail="latest model score" valueTone={tone(predicted - baseValue)} />
        <InfoTile label="Delta" value={(predicted - baseValue).toFixed(4)} detail="prediction minus base" valueTone={tone(predicted - baseValue)} />
      </div>
      <div className="insight-stack">
        <section>
          <h3>Top positive contributors</h3>
          <ExplainTable rows={positive} empty="No positive contributors." />
        </section>
        <section>
          <h3>Top negative anchors</h3>
          <ExplainTable rows={negative} empty="No negative anchors." />
        </section>
      </div>
    </article>
  );
}

function SymbolInsightOverlay({ insight, onClose }) {
  useEffect(() => {
    if (!insight) return undefined;
    const onKey = (event) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [insight, onClose]);

  if (!insight) return null;
  const hasSentiment = Boolean(insight.sentiment);
  const hasExplain = Boolean(insight.explain);
  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <section className="symbol-modal" role="dialog" aria-modal="true" aria-label={`${insight.symbol} insight`} onMouseDown={(event) => event.stopPropagation()}>
        <header className="symbol-modal-head">
          <div>
            <span className="eyebrow">Symbol insight</span>
            <h2>{insight.symbol}</h2>
          </div>
          <button type="button" className="icon-button" onClick={onClose} aria-label="Close symbol insight">
            x
          </button>
        </header>
        {insight.loading && (
          <div className="insight-loading">
            <span className="spinner" />
            <span>Loading explanation and sentiment</span>
          </div>
        )}
        {insight.error && <p className="error-text">{insight.error}</p>}
        {!insight.loading && !hasSentiment && !hasExplain && !insight.error && <p className="muted">No insight data available.</p>}
        {hasSentiment && <SentimentSummary payload={insight.sentiment} />}
        {hasExplain && <ExplainSummary payload={insight.explain} />}
      </section>
    </div>
  );
}

function OrdersView({ rows, syncOrders, tableLimits }) {
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
        <OrderTable rows={rows} paged tableLimits={tableLimits} />
      </article>
    </div>
  );
}

function DataView({ status, suggestions, watchlist, movers }) {
  const s = dataOf(status);
  const suggestionRows = extractSuggestions(suggestions);
  const watchRows = extractWatchlist(watchlist);
  const moverRows = extractMovers(movers);
  const suggestionsLoading = !suggestions;
  const watchlistLoading = !watchlist;
  const moversLoading = !movers;
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
          <PagedDataTable
            rows={suggestionRows}
            empty={suggestionsLoading ? "Loading suggestions..." : "No suggestions found."}
            initial={DASHBOARD_DATA_FALLBACK_INITIAL_ROWS}
            step={DASHBOARD_DATA_FALLBACK_PAGE_ROWS}
            columns={[
              { label: "Rank", value: (r) => text(r.rank, "-") },
              { label: "Symbol", value: (r) => text(r.symbol, "-") },
              { label: "Score", value: (r) => text(firstDefined(r.score, r.suggest_score, r.ml_score), "-") },
              { label: "Confidence", value: (r) => text(r.confidence, "-") },
              { label: "Close", value: (r) => money(r.close) },
              { label: "Change", value: (r) => pct(r.change_pct), className: (r) => tone(r.change_pct) },
            { label: "Signals", value: (r) => arrayFrom(r.signals).join(", ") || "-" },
          ]}
        />
      </article>
      <article className="surface">
        <div className="section-head">
          <h2>Watchlist</h2>
          <span className="status-pill">{watchRows.length}</span>
        </div>
        <PagedDataTable
          rows={watchRows}
          empty={watchlistLoading ? "Loading watchlist..." : "No watchlist rows."}
          initial={DASHBOARD_DATA_FALLBACK_INITIAL_ROWS}
          step={DASHBOARD_DATA_FALLBACK_PAGE_ROWS}
          columns={[
            { label: "Symbol", value: (r) => text(r.symbol, "-") },
            { label: "Score", value: (r) => text(firstDefined(r.score, r.suggest_score, r.ml_score, r.confidence), "-") },
            { label: "Close", value: (r) => money(r.close) },
            { label: "Change", value: (r) => pct(r.change_pct), className: (r) => tone(r.change_pct) },
            { label: "Volume", value: (r) => `${number(r.volume_ratio).toFixed(2)}x` },
            { label: "Signals", value: (r) => arrayFrom(r.signals).slice(0, 4).join(", ") || "-" },
          ]}
        />
      </article>
      <article className="surface">
        <div className="section-head">
          <h2>Movers</h2>
          <span className="status-pill">{moverRows.length}</span>
        </div>
        <PagedDataTable
          rows={moverRows}
          empty={moversLoading ? "Loading movers..." : "No movers found."}
          initial={DASHBOARD_DATA_FALLBACK_INITIAL_ROWS}
          step={DASHBOARD_DATA_FALLBACK_PAGE_ROWS}
          columns={[
            { label: "Side", value: (r) => text(r.direction, "-") },
            { label: "Symbol", value: (r) => text(r.symbol, "-") },
            { label: "Price", value: (r) => money(r.price ?? r.close) },
            { label: "Change $", value: (r) => money(r.change), className: (r) => tone(r.change) },
            { label: "Change", value: (r) => pct(r.change_pct ?? r.percent_change), className: (r) => tone(r.change_pct ?? r.percent_change) },
          ]}
        />
      </article>
    </div>
  );
}

function ComplianceView({
  wash,
  pdt,
  tax,
  taxError,
  taxYear,
  setTaxYear,
  taxAccount,
  setTaxAccount,
  accounts,
  loadTax,
  tableLimits,
}) {
  const washRows = aggregateWashRows(extractWash(wash));
  const paperWashRows = washRows.filter((row) => row.tax_universe === "paper");
  const realWashRows = washRows.filter((row) => row.tax_universe !== "paper");
  const pdtData = dataOf(pdt);
  const taxData = dataOf(tax);
  const taxSummary = taxData.consolidated || arrayFrom(taxData.by_account)[0] || taxData;
  const taxAmount = taxSummary.estimated_federal_tax || {};
  const quarterRows = taxQuarterRows(tax);
  const details = taxDetailRows(tax);
  const washColumns = [
    { label: "Symbol", value: (r) => text(r.symbol, "-") },
    { label: "Sold", value: (r) => dateOnlyText(firstDefined(r.sell_date, r.sell_timestamp_utc, r.sold_at, r.sold_date, r.date)) },
    { label: "Accounts", value: (r) => text(r.account_refs || r.account_ref, "-") },
    { label: "Events", value: (r) => text(r.sell_count, "1") },
    { label: "Loss", value: (r) => money(r.loss_amount ?? r.loss) },
    { label: "Window End", value: (r) => dateOnlyText(firstDefined(r.wash_window_end, r.window_end, r.window_end_date, r.expires_at, r.expiration_date)) },
    { label: "Universe", value: (r) => text(firstDefined(r.tax_universe, r.universe, r.account_mode), "-") },
  ];
  const [activeComplianceTab, setActiveComplianceTab] = useState("wash");
  const loadTaxRef = useRef(loadTax);
  useEffect(() => {
    loadTaxRef.current = loadTax;
  }, [loadTax]);
  useEffect(() => {
    if (activeComplianceTab !== "taxes" || !/^\d{4}$/.test(taxYear)) return undefined;
    const timer = window.setTimeout(() => {
      loadTaxRef.current(taxYear, taxAccount);
    }, 350);
    return () => window.clearTimeout(timer);
  }, [activeComplianceTab, taxYear, taxAccount]);
  return (
    <div className="section-layout">
      <div className="sub-tabs" aria-label="Compliance sections">
        <button
          type="button"
          className={activeComplianceTab === "wash" ? "active" : ""}
          onClick={() => setActiveComplianceTab("wash")}
        >
          Wash Sale
        </button>
        <button
          type="button"
          className={activeComplianceTab === "taxes" ? "active" : ""}
          onClick={() => setActiveComplianceTab("taxes")}
        >
          Taxes
        </button>
      </div>

      {activeComplianceTab === "wash" && (
        <>
          <article className="surface">
            <div className="section-head">
              <div>
                <span className="eyebrow">Wash sale and PDT</span>
                <h2>Wash Sale</h2>
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
            <PagedDataTable
              rows={paperWashRows}
              columns={washColumns}
              empty="No active paper wash-sale windows."
              initial={tableLimits?.tableInitialRows}
              step={tableLimits?.tablePageRows}
            />
          </article>
          <article className="surface">
            <div className="section-head">
              <div>
                <span className="eyebrow">IRS 1091</span>
                <h2>Active Wash Sale Windows - Real</h2>
              </div>
              <span className="status-pill">{realWashRows.length}</span>
            </div>
            <PagedDataTable
              rows={realWashRows}
              columns={washColumns}
              empty="No active real wash-sale windows."
              initial={tableLimits?.tableInitialRows}
              step={tableLimits?.tablePageRows}
            />
          </article>
        </>
      )}

      {activeComplianceTab === "taxes" && (
        <article className="surface">
          <div className="section-head">
            <div>
              <span className="eyebrow">Federal estimate</span>
              <h2>Taxes</h2>
            </div>
            <div className="symbol-form tax-form">
              <input value={taxYear} onChange={(event) => setTaxYear(event.target.value.replace(/\D/g, "").slice(0, 4))} aria-label="Tax year" />
              <select value={taxAccount} onChange={(event) => setTaxAccount(event.target.value)} aria-label="Tax account">
                <option value="">All real accounts</option>
                {accounts.map((account) => (
                  <option key={account.selector} value={account.selector}>
                    {account.selector}
                  </option>
                ))}
              </select>
            </div>
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
              <span className="eyebrow">Default view</span>
              <h2>Quarter Breakdown</h2>
            </div>
            <span className="status-pill">{quarterRows.length}</span>
          </div>
          <PagedDataTable
            rows={quarterRows}
            empty="No quarterly tax estimate loaded."
            initial={4}
            step={4}
            columns={[
              { label: "Quarter", value: (r) => `Q${text(r.quarter, "-")}` },
              { label: "Period", value: (r) => `${dateOnlyText(r.period_start)} to ${dateOnlyText(r.period_end)}` },
              { label: "Short Net", value: (r) => money(r.short_term?.net ?? r.short_term_net ?? r.short_net), className: (r) => tone(r.short_term?.net ?? r.short_term_net ?? r.short_net) },
              { label: "Long Net", value: (r) => money(r.long_term?.net ?? r.long_term_net ?? r.long_net), className: (r) => tone(r.long_term?.net ?? r.long_term_net ?? r.long_net) },
              { label: "Tax", value: (r) => money(r.estimated_federal_tax?.total ?? r.total_tax ?? r.estimated_federal_tax) },
              { label: "Scope", value: (r) => text(r.scope, "-") },
            ]}
          />
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
            initial={tableLimits?.tableInitialRows}
            step={tableLimits?.tablePageRows}
            columns={[
              { label: "Exit", value: (r) => dateOnlyText(r.exit_date) },
              { label: "Account", value: (r) => `${text(r.provider, "-")}:${text(r.account_ref, "-")}` },
              { label: "Origin", value: (r) => text(r.execution_origin, "-") },
              { label: "Symbol", value: (r) => text(r.symbol, "-") },
              { label: "Qty", value: (r) => number(r.qty).toFixed(2) },
              { label: "Term", value: (r) => text(r.term, "-") },
              { label: "P&L", value: (r) => money(r.pnl), className: (r) => tone(r.pnl) },
              { label: "Tax Impact", value: (r) => money(r.estimated_federal_tax_impact), className: (r) => tone(r.estimated_federal_tax_impact) },
            ]}
          />
        </article>
      )}
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
  const [activeTab, setActiveTab] = useState(initialDashboardTab);
  const [state, setState] = useState(defaultState);
  const [errors, setErrors] = useState({});
  const [status, setStatus] = useState("Loading snapshot");
  const [taxYear, setTaxYear] = useState(String(new Date().getFullYear()));
  const [taxAccount, setTaxAccount] = useState("");
  const [selectedAccount, setSelectedAccountState] = useState(storedDashboardAccount);
  const [chartRange, setChartRange] = useState({ mode: "1d", start: "", end: "" });
  const [positionBars, setPositionBars] = useState({});
  const [barLoadingKeys, setBarLoadingKeys] = useState(new Set());
  const [barRefreshSeq, setBarRefreshSeq] = useState(0);
  const [realtimeStatus, setRealtimeStatus] = useState("Snapshot polling");
  const [symbolInsight, setSymbolInsight] = useState(null);
  const refreshInFlight = useRef(false);
  const realtimeConnected = useRef(false);
  const realtimeRefreshInFlight = useRef(false);
  const barsInFlight = useRef(new Set());
  const barsRefreshSeen = useRef(new Map());
  const taxYearRef = useRef(taxYear);
  const taxAccountRef = useRef(taxAccount);

  const allAccounts = useMemo(() => accountRows(state.accounts), [state.accounts]);
  const allPositions = useMemo(() => positionRows(state.positions), [state.positions]);
  const allOrders = useMemo(() => orderRows(state.orders), [state.orders]);
  const filteredAuto = useMemo(() => filterAutoByAccount(state.auto, selectedAccount), [state.auto, selectedAccount]);
  const accounts = useMemo(() => filterRowsByAccount(allAccounts, selectedAccount), [allAccounts, selectedAccount]);
  const positions = useMemo(() => filterRowsByAccount(allPositions, selectedAccount), [allPositions, selectedAccount]);
  const orders = useMemo(() => filterRowsByAccount(allOrders, selectedAccount), [allOrders, selectedAccount]);
  const mlqLookup = useMemo(() => mlqIndex(filteredAuto), [filteredAuto]);
  const chartSpec = useMemo(() => chartSpecFromRange(chartRange), [chartRange]);
  const tableLimits = useMemo(() => dashboardLimitsFor(state.apiLimits), [state.apiLimits]);
  const marketBarsBatchSize = useMemo(
    () => marketBarsBatchSizeFor(state.apiLimits, chartSpec),
    [state.apiLimits, chartSpec]
  );
  const selectTab = useCallback((tab) => {
    const next = normalizeTab(tab);
    setActiveTab(next);
    persistDashboardTab(next);
  }, []);
  const selectAccount = useCallback((selector) => {
    setSelectedAccountState(selector);
    persistDashboardAccount(selector);
  }, []);
  const closeSymbolInsight = useCallback(() => setSymbolInsight(null), []);
  const openSymbolInsight = useCallback(async (symbolValue) => {
    const symbol = text(symbolValue, "").toUpperCase();
    if (!symbol) return;
    setSymbolInsight({ symbol, loading: true, sentiment: null, explain: null, error: null });
    const [sentimentResult, explainResult] = await Promise.allSettled([
      api(`/feeds/sentiment/${encodeURIComponent(symbol)}`, { timeoutMs: 180000 }),
      api(`/ml/explain/${encodeURIComponent(symbol)}`, { timeoutMs: 180000 }),
    ]);
    const failures = [];
    const sentiment = sentimentResult.status === "fulfilled" ? sentimentResult.value : null;
    const explain = explainResult.status === "fulfilled" ? explainResult.value : null;
    if (sentimentResult.status === "rejected") failures.push(`sentiment: ${sentimentResult.reason?.message || sentimentResult.reason}`);
    if (explainResult.status === "rejected") failures.push(`explain: ${explainResult.reason?.message || explainResult.reason}`);
    setSymbolInsight((current) => {
      if (!current || current.symbol !== symbol) return current;
      return {
        symbol,
        loading: false,
        sentiment,
        explain,
        error: failures.join("; ") || null,
      };
    });
  }, []);

  useEffect(() => {
    persistDashboardTab(activeTab);
  }, [activeTab]);

  useEffect(() => {
    const onHashChange = () => {
      setActiveTab(initialDashboardTab());
    };
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  useEffect(() => {
    if (selectedAccount && allAccounts.length && !allAccounts.some((account) => account.selector === selectedAccount)) {
      selectAccount("");
    }
  }, [allAccounts, selectedAccount, selectAccount]);

  useEffect(() => {
    taxYearRef.current = taxYear;
  }, [taxYear]);

  useEffect(() => {
    taxAccountRef.current = taxAccount;
  }, [taxAccount]);

  useEffect(() => {
    setTaxAccount(selectedAccount || "");
  }, [selectedAccount]);

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

  function markBarsLoading(keys, loading) {
    setBarLoadingKeys((current) => {
      const next = new Set(current);
      keys.forEach((key) => {
        if (loading) {
          next.add(key);
        } else {
          next.delete(key);
        }
      });
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
    const params = new URLSearchParams({ year, quarter: "1-4", details: "true" });
    if (account) params.set("account", account);
    await loadResource("tax", `/compliance/tax?${params.toString()}`, { timeoutMs: 180000 });
    setStatus(`Loaded tax ${year}`);
  }

  async function syncOrders() {
    setStatus("Syncing provider orders");
    await loadResource("syncOrders", "/auto/sync-orders", { timeoutMs: 180000 });
    await Promise.all([
      loadResource("orders", `/trade/orders?limit=${tableLimits.ordersLimit}&sync=true`, { timeoutMs: 180000 }),
      loadResource("positions", "/trade/positions?sync=true", { timeoutMs: 180000 }),
      loadResource("auto", "/auto/status"),
      loadResource("wash", "/compliance/wash"),
      loadResource("pdt", "/compliance/pdt"),
    ]);
    setStatus(`Synced ${clientTimeText()}`);
  }

  async function refreshAll({ silent = false, full = false } = {}) {
    if (refreshInFlight.current) return;
    refreshInFlight.current = true;
    if (!silent) setStatus("Loading latest snapshot");
    const limitsPayload = await loadResource("apiLimits", "/limits");
    const activeTableLimits = dashboardLimitsFor(limitsPayload || state.apiLimits);
    const requests = [
      ["accounts", "/trade/account"],
      ["positions", "/trade/positions?sync=false", { timeoutMs: 180000 }],
      ["orders", `/trade/orders?limit=${activeTableLimits.ordersLimit}&sync=false`, { timeoutMs: 180000 }],
      ["auto", "/auto/status"],
      ["autoHistory", "/auto/history"],
      ["wash", "/compliance/wash"],
      ["pdt", "/compliance/pdt"],
    ];
    if (full) {
      const taxParams = new URLSearchParams({ year: taxYearRef.current, quarter: "1-4", details: "true" });
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
      setBarRefreshSeq((current) => current + 1);
      const errorCount = Object.keys(errors).length;
      setStatus(`${errorCount ? `${errorCount} errors, ` : ""}Updated ${clientTimeText()}`);
    } finally {
      refreshInFlight.current = false;
    }
  }

  useEffect(() => {
    refreshAll({ full: true }).catch((err) => setStatus(err.message));
    const liveTimer = window.setInterval(() => {
      if (realtimeConnected.current) return;
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
    if (!window.EventSource) {
      realtimeConnected.current = false;
      setRealtimeStatus("Snapshot polling");
      return undefined;
    }
    let closed = false;
    const source = new window.EventSource("/events/stream");
    const markConnected = (payload) => {
      realtimeConnected.current = true;
      setRealtimeStatus(realtimeLabel(payload));
    };
    source.addEventListener("connected", (event) => {
      try {
        markConnected(JSON.parse(event.data || "{}"));
      } catch {
        markConnected({});
      }
    });
    source.addEventListener("dashboard.refresh", (event) => {
      let payload = {};
      try {
        payload = JSON.parse(event.data || "{}");
      } catch {
        payload = {};
      }
      markConnected(payload);
      if (realtimeRefreshInFlight.current) return;
      realtimeRefreshInFlight.current = true;
      refreshAll({ silent: true })
        .catch((err) => setStatus(err.message))
        .finally(() => {
          realtimeRefreshInFlight.current = false;
        });
    });
    source.onerror = () => {
      if (closed) return;
      realtimeConnected.current = false;
      setRealtimeStatus("Snapshot polling");
    };
    return () => {
      closed = true;
      realtimeConnected.current = false;
      source.close();
    };
    // Connect once; polling remains active as a fallback when the stream is down.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const symbols = Array.from(new Set(positions.map((row) => text(row.symbol, "").toUpperCase()).filter(Boolean)));
    const missing = symbols.filter((symbolName) => {
      const key = barCacheKey(symbolName, chartSpec);
      const hasBars = Boolean(positionBars[key]);
      const needsBackgroundRefresh = hasBars && barRefreshSeq > 0 && barsRefreshSeen.current.get(key) !== barRefreshSeq;
      return (!hasBars || needsBackgroundRefresh) && !barsInFlight.current.has(key);
    });
    if (!missing.length) return undefined;
    let cancelled = false;
    async function loadMissingBars() {
      const chunks = chunkArray(missing, marketBarsBatchSize);
      const workers = Array.from({ length: Math.min(POSITION_BAR_WORKERS, chunks.length) }, async (_, workerIndex) => {
        for (let index = workerIndex; index < chunks.length; index += POSITION_BAR_WORKERS) {
          const chunk = chunks[index];
          const keys = chunk.map((symbolName) => barCacheKey(symbolName, chartSpec));
          keys.forEach((key) => barsInFlight.current.add(key));
          markBarsLoading(keys, true);
          try {
            const params = new URLSearchParams({
              symbols: chunk.join(","),
              timeframe: chartSpec.timeframe,
              limit: String(chartSpec.limit),
              start: chartSpec.startIso,
              end: chartSpec.endIso,
            });
            const payload = await api(`/market/bars?${params.toString()}`, { timeoutMs: 120000 });
            if (!cancelled) {
              const results = dataOf(payload).results || {};
              setPositionBars((current) => {
                const next = { ...current };
                for (const symbolName of chunk) {
                  const result = results[symbolName] || results[text(symbolName, "").toUpperCase()];
                  if (result) {
                    next[barCacheKey(symbolName, chartSpec)] = result;
                  }
                }
                return next;
              });
            }
          } catch (err) {
            setResourceError(`bars:${chunk.join(",")}`, err);
          } finally {
            keys.forEach((key) => barsInFlight.current.delete(key));
            keys.forEach((key) => barsRefreshSeen.current.set(key, barRefreshSeq));
            markBarsLoading(keys, false);
          }
        }
      });
      await Promise.all(workers);
    }
    loadMissingBars();
    return () => {
      cancelled = true;
    };
  }, [positions, positionBars, chartSpec, marketBarsBatchSize, barRefreshSeq]);

  return (
    <div className="app-shell">
      <Sidebar activeTab={activeTab} setActiveTab={selectTab} />
      <div className="workspace">
        <header className="topbar">
          <div className="topbar-left">
            <div>
              <span className="eyebrow">Trading snapshot dashboard</span>
              <h1>mlai-trade</h1>
            </div>
            <AccountFilter accounts={allAccounts} selectedAccount={selectedAccount} setSelectedAccount={selectAccount} />
          </div>
          <div className="toolbar">
            <ChartRangeControls range={chartRange} setRange={setChartRange} />
            <span className="status-pill">Bars {chartSpec.timeframe}</span>
            <span className="status-pill">TZ {CLIENT_TIME_ZONE}</span>
            <span className="status-pill">{realtimeStatus}</span>
            <span className="status-pill">{status}</span>
            {!isLocalhostAccess() && (
              <form className="logout-form" method="post" action="/logout">
                <button type="submit" className="secondary-button">
                  Logout
                </button>
              </form>
            )}
          </div>
        </header>
        <MobileTabs activeTab={activeTab} setActiveTab={selectTab} />
        <main>
          <section className={`panel ${activeTab === "overview" ? "active" : ""}`}>
            <Overview
              accounts={accounts}
              positions={positions}
              orders={orders}
              auto={filteredAuto}
              autoHistory={state.autoHistory}
              mlqLookup={mlqLookup}
              chartSpec={chartSpec}
              barsBySymbol={positionBars}
              barLoadingKeys={barLoadingKeys}
            />
          </section>
          <section className={`panel ${activeTab === "accounts" ? "active" : ""}`}>
            <AccountsView
              rows={accounts}
              positions={positions}
              barsBySymbol={positionBars}
              chartSpec={chartSpec}
              barLoadingKeys={barLoadingKeys}
            />
          </section>
          <section className={`panel ${activeTab === "positions" ? "active" : ""}`}>
            <PositionsView
              positions={positions}
              auto={filteredAuto}
              mlqLookup={mlqLookup}
              barsBySymbol={positionBars}
              chartSpec={chartSpec}
              tableLimits={tableLimits}
              barLoadingKeys={barLoadingKeys}
              onSymbolClick={openSymbolInsight}
            />
          </section>
          <section className={`panel ${activeTab === "orders" ? "active" : ""}`}>
            <OrdersView rows={orders} syncOrders={syncOrders} tableLimits={tableLimits} />
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
              tableLimits={tableLimits}
            />
          </section>
        </main>
      </div>
      <SymbolInsightOverlay insight={symbolInsight} onClose={closeSymbolInsight} />
    </div>
  );
}

createRoot(document.getElementById("root")).render(
  <ErrorBoundary>
    <App />
  </ErrorBoundary>
);
