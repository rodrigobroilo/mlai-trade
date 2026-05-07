import React, { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

const tabs = [
  ["overview", "Overview"],
  ["accounts", "Accounts"],
  ["positions", "Positions"],
  ["orders", "Orders"],
  ["auto", "Auto"],
  ["market", "Market"],
  ["ml", "ML"],
  ["data", "Data"],
  ["compliance", "Compliance"],
  ["feeds", "Feeds"],
  ["system", "System"],
];

const defaultState = {
  health: null,
  routes: null,
  daemon: null,
  accounts: null,
  positions: null,
  orders: null,
  auto: null,
  autoHistory: null,
  autoConfig: null,
  ml: null,
  mlExplainable: null,
  mlExplained: null,
  dataStatus: null,
  suggestions: null,
  watchlist: null,
  movers: null,
  wash: null,
  pdt: null,
  feedsStatus: null,
  feedsList: null,
  marketClock: null,
  marketCalendar: null,
  quote: null,
  bars: null,
  news: null,
  explain: null,
  feedSentiment: null,
  feedGraph: null,
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
  return payload.data !== undefined ? payload.data : payload;
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

function extractBars(payload) {
  const data = dataOf(payload);
  const bars = data.bars ?? data.rows ?? data;
  if (Array.isArray(bars)) return bars;
  if (bars && typeof bars === "object") return Object.values(bars).flat().filter(Boolean);
  return [];
}

function extractNews(payload) {
  const data = dataOf(payload);
  return arrayFrom(data.news || data.articles || data.recent || data);
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

function extractFeedList(payload) {
  const data = dataOf(payload);
  const rows = arrayFrom(data);
  return rows.map((row) => (typeof row === "string" ? { symbol: row } : row));
}

function explainRows(payload) {
  const data = dataOf(payload);
  return arrayFrom(data.contributions || data.features || data.shap_values || data.rows || data.explanation);
}

function routeRows(payload) {
  const data = dataOf(payload);
  return arrayFrom(data.routes || data.sections || data);
}

function jsonSummary(value, max = 80) {
  const raw = JSON.stringify(value);
  if (!raw) return "not available";
  return raw.length > max ? `${raw.slice(0, max)}...` : raw;
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
  if (!rows.length) return <p className="muted">{empty}</p>;
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
          {rows.map((row, idx) => (
            <tr key={`${row.account_selector || row.account || ""}:${row.symbol || row.id || row.path || ""}:${idx}`}>
              {columns.map((col) => (
                <td key={col.label} className={col.className ? col.className(row) : ""}>
                  {col.value(row)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
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

function LineChart({ values, color = "#285fd4", height = 260 }) {
  const ref = useRef(null);
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
    if (values.length < 2) {
      ctx.fillStyle = "#657287";
      ctx.font = `${13 * ratio}px system-ui`;
      ctx.textAlign = "center";
      ctx.fillText("No real series loaded", width / 2, realHeight / 2);
      return;
    }

    const min = Math.min(...values);
    const max = Math.max(...values);
    const span = Math.max(max - min, 0.0001);
    const pad = 22 * ratio;

    ctx.strokeStyle = "#dfe6ef";
    ctx.lineWidth = ratio;
    for (let i = 0; i < 5; i += 1) {
      const y = pad + ((realHeight - pad * 2) * i) / 4;
      ctx.beginPath();
      ctx.moveTo(pad, y);
      ctx.lineTo(width - pad, y);
      ctx.stroke();
    }

    const points = values.map((v, i) => {
      const x = pad + (i / (values.length - 1)) * (width - pad * 2);
      const y = realHeight - pad - ((v - min) / span) * (realHeight - pad * 2);
      return [x, y];
    });
    const gradient = ctx.createLinearGradient(0, pad, 0, realHeight - pad);
    gradient.addColorStop(0, "rgba(40, 95, 212, 0.2)");
    gradient.addColorStop(1, "rgba(40, 95, 212, 0)");

    ctx.beginPath();
    points.forEach(([x, y], i) => (i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y)));
    [...points].reverse().forEach(([x]) => ctx.lineTo(x, realHeight - pad));
    ctx.closePath();
    ctx.fillStyle = gradient;
    ctx.fill();

    ctx.beginPath();
    points.forEach(([x, y], i) => (i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y)));
    ctx.strokeStyle = color;
    ctx.lineWidth = 3 * ratio;
    ctx.stroke();
    const last = points[points.length - 1];
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.arc(last[0], last[1], 4 * ratio, 0, Math.PI * 2);
    ctx.fill();
  }, [values, color, height]);

  return <canvas ref={ref} className="chart" style={{ height }} />;
}

function DonutChart({ rows }) {
  const ref = useRef(null);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    const ratio = window.devicePixelRatio || 1;
    const width = Math.floor((canvas.clientWidth || 180) * ratio);
    const height = Math.floor((canvas.clientHeight || 180) * ratio);
    canvas.width = width;
    canvas.height = height;
    ctx.clearRect(0, 0, width, height);
    const values = rows.slice(0, 8).map((row) => Math.max(0, number(positionMarketValue(row))));
    const total = values.reduce((sum, value) => sum + value, 0);
    if (!total) {
      ctx.fillStyle = "#657287";
      ctx.font = `${13 * ratio}px system-ui`;
      ctx.textAlign = "center";
      ctx.fillText("No positions", width / 2, height / 2);
      return;
    }
    const colors = ["#285fd4", "#20b8d3", "#0c8f55", "#e37b24", "#7c5cc4", "#c6362f", "#4f6f52", "#8750a6"];
    const cx = width / 2;
    const cy = height / 2;
    const radius = Math.min(width, height) * 0.42;
    const inner = radius * 0.58;
    let start = -Math.PI / 2;
    values.forEach((value, index) => {
      const end = start + (value / total) * Math.PI * 2;
      ctx.beginPath();
      ctx.arc(cx, cy, radius, start, end);
      ctx.arc(cx, cy, inner, end, start, true);
      ctx.closePath();
      ctx.fillStyle = colors[index % colors.length];
      ctx.fill();
      start = end;
    });
    ctx.fillStyle = "#172033";
    ctx.font = `${13 * ratio}px system-ui`;
    ctx.textAlign = "center";
    ctx.fillText("Symbols", cx, cy - 3 * ratio);
    ctx.font = `700 ${15 * ratio}px system-ui`;
    ctx.fillText(String(rows.length), cx, cy + 17 * ratio);
  }, [rows]);
  return <canvas ref={ref} className="donut" width="180" height="180" />;
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

function PositionTable({ rows, empty }) {
  return (
    <DataTable
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
        { label: "MLQ", value: (r) => r.ml_quintile || r.ml_quantile || r.mlq || "-" },
      ]}
    />
  );
}

function OrderTable({ rows }) {
  return (
    <DataTable
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

function Overview({ accounts, positions, orders, auto, bars, selectedSymbol }) {
  const managed = autoManagedRows(auto);
  const autoAccountsRows = autoAccounts(auto);
  const equity = accounts.reduce((sum, row) => sum + number(row.equity), 0);
  const cash = accounts.reduce((sum, row) => sum + number(row.cash), 0);
  const buyingPower = accounts.reduce((sum, row) => sum + number(row.buying_power), 0);
  const openValue = positions.reduce((sum, row) => sum + number(positionMarketValue(row)), 0);
  const unrealized = positions.reduce((sum, row) => sum + number(positionPnl(row)), 0);
  const autoPnl = managed.reduce((sum, row) => sum + number(positionPnl(row)), 0);
  const closedPnl = autoAccountsRows.reduce((sum, row) => sum + number(row.closed_pnl), 0);
  const barValues = extractBars(bars)
    .map((row) => number(row.close ?? row.c))
    .filter((value) => value > 0);

  return (
    <div className="dashboard-grid">
      <article className="surface balance-panel">
        <div className="section-head">
          <div>
            <span className="eyebrow">Real market bars</span>
            <h2>{selectedSymbol} price</h2>
          </div>
          <strong>{barValues.length ? money(barValues[barValues.length - 1]) : "not available"}</strong>
        </div>
        <LineChart values={barValues} />
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
        <article className="surface allocation-card">
          <div className="section-head compact">
            <h2>Allocation</h2>
            <span>{positions.length} provider positions</span>
          </div>
          <DonutChart rows={positions} />
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

      <article className="surface table-panel">
        <div className="section-head">
          <div>
            <span className="eyebrow">Provider source</span>
            <h2>Open Positions</h2>
          </div>
          <span className="status-pill">{positions.length}</span>
        </div>
        <PositionTable rows={positions.slice(0, 18)} />
      </article>

      <article className="surface table-panel">
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

function AccountsView({ rows, raw }) {
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
      <JsonPanel title="Raw account API response" value={raw} />
    </div>
  );
}

function PositionsView({ positions, auto }) {
  const managed = autoManagedRows(auto);
  const unmanaged = autoUnmanagedRows(auto);
  return (
    <div className="section-layout">
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Provider query</span>
            <h2>All Open Positions</h2>
          </div>
          <span className="status-pill">{positions.length}</span>
        </div>
        <PositionTable rows={positions} />
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Auto rules</span>
            <h2>Tracked vs Not Tracked</h2>
          </div>
          <span className="status-pill">{managed.length} tracked / {unmanaged.length} not tracked</span>
        </div>
        <PositionTable rows={[...managed, ...unmanaged]} />
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
        <OrderTable rows={rows} />
      </article>
    </div>
  );
}

function AutoView({ auto, autoHistory, autoConfig }) {
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
        <PositionTable rows={managed} empty="No auto-managed positions." />
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Manual or provider-held</span>
            <h2>Positions Not Tracked By Auto</h2>
          </div>
        </div>
        <PositionTable rows={unmanaged} empty="No provider positions outside auto tracking." />
      </article>
      <JsonPanel title="Auto configuration" value={dataOf(autoConfig)} />
    </div>
  );
}

function MarketView({ symbol, setSymbol, quote, bars, news, clock, calendar, loadMarket }) {
  const cleanQuote = dataOf(quote);
  const quoteData = cleanQuote.quote || cleanQuote;
  const barRows = extractBars(bars);
  const values = barRows.map((p) => number(p.close ?? p.c)).filter((value) => value > 0);
  const newsRows = extractNews(news);
  return (
    <div className="section-layout">
      <article className="surface market-panel">
        <div className="section-head">
          <div>
            <span className="eyebrow">Market data</span>
            <h2>Quote, Bars, News</h2>
          </div>
          <form
            className="symbol-form"
            onSubmit={(event) => {
              event.preventDefault();
              loadMarket(symbol);
            }}
          >
            <input value={symbol} onChange={(event) => setSymbol(event.target.value.toUpperCase())} aria-label="Symbol" />
            <button className="primary">Load</button>
          </form>
        </div>
        <LineChart values={values} color="#20b8d3" height={320} />
        <div className="quote-grid">
          <InfoTile label="Symbol" value={text(quoteData.symbol, symbol)} detail="latest quote" />
          <InfoTile label="Bid" value={money(quoteData.bid_price ?? quoteData.bid)} detail={quoteData.bid_size ? `${quoteData.bid_size} shares` : ""} />
          <InfoTile label="Ask" value={money(quoteData.ask_price ?? quoteData.ask)} detail={quoteData.ask_size ? `${quoteData.ask_size} shares` : ""} />
          <InfoTile label="Bars" value={String(barRows.length)} detail={barRows[0]?.t || barRows[0]?.timestamp || ""} />
        </div>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Provider clock</span>
            <h2>Market Session</h2>
          </div>
        </div>
        <div className="auto-grid">
          <InfoTile label="Clock" value={text(dataOf(clock).status || dataOf(clock).is_open, "not available")} detail={text(dataOf(clock).timestamp, "")} />
          <InfoTile label="Calendar" value={text(dataOf(calendar).market || dataOf(calendar).provider_market, "not available")} detail={jsonSummary(dataOf(calendar), 120)} />
        </div>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">News</span>
            <h2>{symbol} Articles</h2>
          </div>
          <span className="status-pill">{newsRows.length}</span>
        </div>
        <DataTable
          rows={newsRows.slice(0, 20)}
          columns={[
            { label: "Published", value: (r) => dateText(r.published_at || r.created_at || r.updated_at) },
            { label: "Source", value: (r) => text(r.source, "-") },
            { label: "Headline", value: (r) => text(r.title || r.headline, "-") },
            { label: "Sentiment", value: (r) => text(r.sentiment, "-") },
          ]}
        />
      </article>
    </div>
  );
}

function MlView({ ml, explainable, explained, explain, symbol, setSymbol, loadExplain }) {
  const status = dataOf(ml);
  const explainedRows = arrayFrom(dataOf(explained).symbols || dataOf(explained).explained || dataOf(explained));
  const explainableRows = arrayFrom(dataOf(explainable).symbols || dataOf(explainable).explainable || dataOf(explainable));
  const shapRows = explainRows(explain);
  return (
    <div className="section-layout">
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Pipeline</span>
            <h2>ML Status</h2>
          </div>
          <span className="status-pill">{status.next_step ? "action needed" : "ready"}</span>
        </div>
        <div className="ml-grid">
          <InfoTile label="Bars" value={text(status.bars?.rows ?? status.bars, "not available")} detail={text(status.bars?.range, "")} />
          <InfoTile label="Features" value={text(status.features?.rows ?? status.features, "not available")} detail={text(status.features?.symbols, "")} />
          <InfoTile label="Predictions" value={text(status.predictions?.latest_rows ?? status.predictions, "not available")} detail={text(status.predictions?.latest_date, "")} />
          <InfoTile label="SHAP cache" value={text(status.shap?.cached ?? status.shap, "not available")} detail="cached explanations" />
        </div>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Explainability</span>
            <h2>SHAP Explanation</h2>
          </div>
          <form
            className="symbol-form"
            onSubmit={(event) => {
              event.preventDefault();
              loadExplain(symbol);
            }}
          >
            <input value={symbol} onChange={(event) => setSymbol(event.target.value.toUpperCase())} aria-label="ML symbol" />
            <button className="primary">Explain</button>
          </form>
        </div>
        <DataTable
          rows={shapRows.slice(0, 30)}
          empty="Run an explanation for a symbol."
          columns={[
            { label: "Feature", value: (r) => text(r.feature || r.name || r.key, "-") },
            { label: "Contribution", value: (r) => number(r.contribution ?? r.shap ?? r.value).toFixed(6), className: (r) => tone(r.contribution ?? r.shap ?? r.value) },
            { label: "Value", value: (r) => text(r.feature_value ?? r.raw_value ?? r.input_value ?? r.val, "-") },
          ]}
        />
      </article>
      <div className="section-layout two-columns">
        <article className="surface">
          <div className="section-head">
            <h2>Explained Cache</h2>
            <span className="status-pill">{explainedRows.length}</span>
          </div>
          <DataTable rows={explainedRows.slice(0, 100)} columns={[{ label: "Symbol", value: (r) => text(r.symbol || r, "-") }, { label: "Date", value: (r) => dateText(r.date || r.explained_at) }]} />
        </article>
        <article className="surface">
          <div className="section-head">
            <h2>Explainable Symbols</h2>
            <span className="status-pill">{explainableRows.length}</span>
          </div>
          <DataTable rows={explainableRows.slice(0, 100)} columns={[{ label: "Symbol", value: (r) => text(r.symbol || r, "-") }, { label: "Date", value: (r) => dateText(r.date || r.latest_date) }]} />
        </article>
      </div>
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

function ComplianceView({ wash, pdt, tax, taxYear, setTaxYear, loadTax }) {
  const washRows = extractWash(wash);
  const pdtData = dataOf(pdt);
  const taxData = dataOf(tax);
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
          <InfoTile label="Wash windows" value={text(dataOf(wash).active_count ?? washRows.length, "0")} detail="active symbols" />
          <InfoTile label="Day trades" value={text(pdtData.day_trades_5d ?? pdtData.day_trades, "not available")} detail="rolling 5 business days" />
          <InfoTile label="PDT flag" value={text(pdtData.pattern_day_trader ?? pdtData.alpaca_pdt_flag, "not available")} detail="provider status" />
          <InfoTile label="Remaining" value={text(pdtData.remaining_day_trades, "not available")} detail="before PDT trigger" />
        </div>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">IRS 1091</span>
            <h2>Active Wash Sale Windows</h2>
          </div>
          <span className="status-pill">{washRows.length}</span>
        </div>
        <DataTable
          rows={washRows}
          columns={[
            { label: "Symbol", value: (r) => text(r.symbol, "-") },
            { label: "Sold", value: (r) => dateText(r.sold_at || r.sold_date || r.date) },
            { label: "Loss", value: (r) => money(r.loss_amount ?? r.loss) },
            { label: "Window End", value: (r) => dateText(r.window_end || r.expires_at || r.expiration_date) },
            { label: "Universe", value: (r) => text(r.tax_universe, "-") },
          ]}
        />
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Federal estimate</span>
            <h2>Tax</h2>
          </div>
          <form
            className="symbol-form"
            onSubmit={(event) => {
              event.preventDefault();
              loadTax(taxYear);
            }}
          >
            <input value={taxYear} onChange={(event) => setTaxYear(event.target.value.replace(/\D/g, "").slice(0, 4))} aria-label="Tax year" />
            <button className="primary">Load</button>
          </form>
        </div>
        <div className="auto-grid">
          <InfoTile label="Year" value={text(taxData.year, taxYear)} detail={text(taxData.period, "")} />
          <InfoTile label="Short-term" value={money(taxData.short_term_tax ?? taxData.short_tax)} detail={money(taxData.short_term_net ?? taxData.short_net)} />
          <InfoTile label="Long-term" value={money(taxData.long_term_tax ?? taxData.long_tax)} detail={money(taxData.long_term_net ?? taxData.long_net)} />
          <InfoTile label="Total tax" value={money(taxData.total_tax ?? taxData.estimated_federal_tax)} detail={text(taxData.filing_status, "")} />
        </div>
        <JsonPanel title="Raw tax estimate" value={taxData} />
      </article>
    </div>
  );
}

function FeedsView({ status, list, sentiment, graph, symbol, setSymbol, loadFeedSymbol }) {
  const feedStatus = dataOf(status);
  const rows = extractFeedList(list);
  const sentimentData = dataOf(sentiment);
  const graphData = dataOf(graph);
  return (
    <div className="section-layout">
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">News and relationships</span>
            <h2>Feeds</h2>
          </div>
          <form
            className="symbol-form"
            onSubmit={(event) => {
              event.preventDefault();
              loadFeedSymbol(symbol);
            }}
          >
            <input value={symbol} onChange={(event) => setSymbol(event.target.value.toUpperCase())} aria-label="Feed symbol" />
            <button className="primary">Load</button>
          </form>
        </div>
        <div className="ml-grid">
          <InfoTile label="Subscriptions" value={text(feedStatus.subscriptions, rows.length ? String(rows.length) : "not available")} detail="tracked symbols" />
          <InfoTile label="Articles" value={text(feedStatus.articles, "not available")} detail={text(feedStatus.article_range, "")} />
          <InfoTile label="Relationships" value={text(feedStatus.relationships, "not available")} detail="company graph edges" />
          <InfoTile label="Correlations" value={text(feedStatus.correlations, "not available")} detail="symbol pairs" />
        </div>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Sentiment</span>
            <h2>{text(sentimentData.symbol, symbol)}</h2>
          </div>
        </div>
        <div className="auto-grid">
          <InfoTile label="7d sentiment" value={text(sentimentData.sentiment_7d, "not available")} detail={`${text(sentimentData.articles_7d, "0")} articles`} />
          <InfoTile label="30d sentiment" value={text(sentimentData.sentiment_30d, "not available")} detail={`${text(sentimentData.articles_30d, "0")} articles`} />
          <InfoTile label="SEC 8-K" value={text(sentimentData.sec_8k_count, "0")} detail="recent filings" />
          <InfoTile label="Graph" value={text(graphData.nodes?.length ?? graphData.node_count, "not available")} detail={`${text(graphData.edges?.length ?? graphData.edge_count, "not available")} edges`} />
        </div>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Subscriptions</span>
            <h2>Feed Symbols</h2>
          </div>
          <span className="status-pill">{rows.length}</span>
        </div>
        <DataTable
          rows={rows.slice(0, 250)}
          columns={[
            { label: "Symbol", value: (r) => text(r.symbol || r.ticker || r, "-") },
            { label: "Source", value: (r) => text(r.source || r.subscription_source || r.reason, "-") },
            { label: "Managed", value: (r) => text(r.managed ?? r.is_managed, "-") },
            { label: "Updated", value: (r) => dateText(r.updated_at || r.created_at || r.synced_at) },
          ]}
        />
      </article>
    </div>
  );
}

function SystemView({ health, routes, daemon, errors }) {
  const rows = routeRows(routes);
  const daemonData = dataOf(daemon);
  const errorRows = Object.entries(errors).map(([key, value]) => ({ key, value }));
  return (
    <div className="section-layout">
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Runtime</span>
            <h2>System</h2>
          </div>
        </div>
        <div className="auto-grid">
          <InfoTile label="API" value={text(dataOf(health).status, "not available")} detail={text(dataOf(health).version, "")} />
          <InfoTile label="Daemon" value={text(daemonData.running ?? daemonData.status, "not available")} detail={text(daemonData.pid ? `pid ${daemonData.pid}` : "", "")} />
          <InfoTile label="Routes" value={String(rows.length)} detail="remote API catalog" />
          <InfoTile label="Errors" value={String(errorRows.length)} detail="last refresh" valueTone={errorRows.length ? "loss" : "gain"} />
        </div>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">API support</span>
            <h2>Routes</h2>
          </div>
          <span className="status-pill">{rows.length}</span>
        </div>
        <DataTable
          rows={rows}
          columns={[
            { label: "Section", value: (r) => text(r.section || r.group || r.command, "-") },
            { label: "Method", value: (r) => text(r.method || r.methods, "-") },
            { label: "Path", value: (r) => text(r.path || r.route, "-") },
            { label: "Params", value: (r) => jsonSummary(r.parameters || r.params || r.actions || {}, 120) },
          ]}
        />
      </article>
      <article className="surface">
        <div className="section-head">
          <h2>Refresh Errors</h2>
        </div>
        <DataTable rows={errorRows} columns={[{ label: "Resource", value: (r) => r.key }, { label: "Error", value: (r) => r.value }]} empty="No refresh errors." />
      </article>
      <JsonPanel title="Raw health" value={dataOf(health)} />
      <JsonPanel title="Raw daemon status" value={daemonData} />
    </div>
  );
}

function App() {
  const [activeTab, setActiveTab] = useState("overview");
  const [state, setState] = useState(defaultState);
  const [errors, setErrors] = useState({});
  const [status, setStatus] = useState("Loading");
  const [symbol, setSymbol] = useState("AAPL");
  const [taxYear, setTaxYear] = useState(String(new Date().getFullYear()));
  const [syncBeforeRead, setSyncBeforeRead] = useState(false);

  const accounts = useMemo(() => accountRows(state.accounts), [state.accounts]);
  const positions = useMemo(() => positionRows(state.positions), [state.positions]);
  const orders = useMemo(() => orderRows(state.orders), [state.orders]);

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

  async function loadMarket(nextSymbol = symbol) {
    const clean = nextSymbol.trim().toUpperCase();
    if (!clean) return;
    setSymbol(clean);
    await Promise.all([
      loadResource("quote", `/market/quote/${encodeURIComponent(clean)}`),
      loadResource("bars", `/market/bars/${encodeURIComponent(clean)}?timeframe=1Day&limit=120`),
      loadResource("news", `/market/news/${encodeURIComponent(clean)}`),
    ]);
  }

  async function loadExplain(nextSymbol = symbol) {
    const clean = nextSymbol.trim().toUpperCase();
    if (!clean) return;
    setSymbol(clean);
    setStatus(`Explaining ${clean}`);
    await loadResource("explain", `/ml/explain/${encodeURIComponent(clean)}`, { timeoutMs: 180000 });
    setStatus(`Explained ${clean}`);
  }

  async function loadFeedSymbol(nextSymbol = symbol) {
    const clean = nextSymbol.trim().toUpperCase();
    if (!clean) return;
    setSymbol(clean);
    await Promise.all([
      loadResource("feedSentiment", `/feeds/sentiment/${encodeURIComponent(clean)}`),
      loadResource("feedGraph", `/feeds/graph/${encodeURIComponent(clean)}`),
    ]);
  }

  async function loadTax(year = taxYear) {
    if (!/^\d{4}$/.test(year)) return;
    setStatus(`Loading tax ${year}`);
    await loadResource("tax", `/compliance/tax?year=${encodeURIComponent(year)}`, { timeoutMs: 180000 });
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

  async function refreshAll() {
    setStatus("Refreshing");
    const sync = syncBeforeRead ? "true" : "false";
    const requests = [
      ["health", "/health"],
      ["routes", "/routes"],
      ["daemon", "/daemon/status"],
      ["accounts", "/trade/account"],
      ["positions", `/trade/positions?sync=${sync}`, { timeoutMs: 180000 }],
      ["orders", `/trade/orders?limit=100&sync=${sync}`, { timeoutMs: 180000 }],
      ["auto", "/auto/status"],
      ["autoHistory", "/auto/history"],
      ["autoConfig", "/auto/config"],
      ["ml", "/ml/status"],
      ["mlExplainable", "/ml/explainable"],
      ["mlExplained", "/ml/explained"],
      ["dataStatus", "/data/status"],
      ["suggestions", "/data/suggest"],
      ["watchlist", "/data/watchlist"],
      ["movers", "/data/movers"],
      ["wash", "/compliance/wash"],
      ["pdt", "/compliance/pdt"],
      ["feedsStatus", "/feeds/status"],
      ["feedsList", "/feeds/list"],
      ["marketClock", "/market/clock"],
      ["marketCalendar", "/market/calendar"],
    ];
    await Promise.all(requests.map(([key, path, options]) => loadResource(key, path, options)));
    await Promise.all([loadMarket(symbol), loadFeedSymbol(symbol)]);
    const errorCount = Object.keys(errors).length;
    setStatus(`${errorCount ? `${errorCount} errors, ` : ""}Updated ${new Date().toLocaleTimeString()}`);
  }

  useEffect(() => {
    refreshAll().catch((err) => setStatus(err.message));
    // Run once on first load; refresh remains explicit to avoid surprise trading/account sync loops.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="app-shell">
      <Sidebar activeTab={activeTab} setActiveTab={setActiveTab} accounts={accounts} status={status} />
      <div className="workspace">
        <header className="topbar">
          <div>
            <span className="eyebrow">Live trading dashboard</span>
            <h1>mlai-trade</h1>
          </div>
          <div className="toolbar">
            <label className="sync-toggle">
              <input
                type="checkbox"
                checked={syncBeforeRead}
                onChange={(event) => setSyncBeforeRead(event.target.checked)}
              />
              Sync before read
            </label>
            <span className="status-pill">{status}</span>
            <button className="primary" onClick={refreshAll}>
              Refresh
            </button>
          </div>
        </header>
        <MobileTabs activeTab={activeTab} setActiveTab={setActiveTab} />
        <main>
          <section className={`panel ${activeTab === "overview" ? "active" : ""}`}>
            <Overview accounts={accounts} positions={positions} orders={orders} auto={state.auto} bars={state.bars} selectedSymbol={symbol} />
          </section>
          <section className={`panel ${activeTab === "accounts" ? "active" : ""}`}>
            <AccountsView rows={accounts} raw={state.accounts} />
          </section>
          <section className={`panel ${activeTab === "positions" ? "active" : ""}`}>
            <PositionsView positions={positions} auto={state.auto} />
          </section>
          <section className={`panel ${activeTab === "orders" ? "active" : ""}`}>
            <OrdersView rows={orders} syncOrders={syncOrders} />
          </section>
          <section className={`panel ${activeTab === "auto" ? "active" : ""}`}>
            <AutoView auto={state.auto} autoHistory={state.autoHistory} autoConfig={state.autoConfig} />
          </section>
          <section className={`panel ${activeTab === "market" ? "active" : ""}`}>
            <MarketView
              symbol={symbol}
              setSymbol={setSymbol}
              quote={state.quote}
              bars={state.bars}
              news={state.news}
              clock={state.marketClock}
              calendar={state.marketCalendar}
              loadMarket={loadMarket}
            />
          </section>
          <section className={`panel ${activeTab === "ml" ? "active" : ""}`}>
            <MlView
              ml={state.ml}
              explainable={state.mlExplainable}
              explained={state.mlExplained}
              explain={state.explain}
              symbol={symbol}
              setSymbol={setSymbol}
              loadExplain={loadExplain}
            />
          </section>
          <section className={`panel ${activeTab === "data" ? "active" : ""}`}>
            <DataView status={state.dataStatus} suggestions={state.suggestions} watchlist={state.watchlist} movers={state.movers} />
          </section>
          <section className={`panel ${activeTab === "compliance" ? "active" : ""}`}>
            <ComplianceView wash={state.wash} pdt={state.pdt} tax={state.tax} taxYear={taxYear} setTaxYear={setTaxYear} loadTax={loadTax} />
          </section>
          <section className={`panel ${activeTab === "feeds" ? "active" : ""}`}>
            <FeedsView
              status={state.feedsStatus}
              list={state.feedsList}
              sentiment={state.feedSentiment}
              graph={state.feedGraph}
              symbol={symbol}
              setSymbol={setSymbol}
              loadFeedSymbol={loadFeedSymbol}
            />
          </section>
          <section className={`panel ${activeTab === "system" ? "active" : ""}`}>
            <SystemView health={state.health} routes={state.routes} daemon={state.daemon} errors={errors} />
          </section>
        </main>
      </div>
    </div>
  );
}

createRoot(document.getElementById("root")).render(<App />);
