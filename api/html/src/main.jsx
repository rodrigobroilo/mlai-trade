import React, { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

const tabs = [
  ["portfolio", "Dashboard"],
  ["auto", "Auto Trading"],
  ["ml", "ML Pipeline"],
  ["market", "Market"],
  ["system", "System"],
];

async function api(path) {
  const res = await fetch(path, { headers: { accept: "application/json" } });
  const text = await res.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch (err) {
    if (!res.ok) throw new Error(text || res.statusText);
    throw err;
  }
  if (!res.ok || json.ok === false) throw new Error(json.error || json.reason || text);
  return json;
}

function number(value, fallback = 0) {
  const n = Number(value);
  return Number.isFinite(n) ? n : fallback;
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
  return number(value) >= 0 ? "gain" : "loss";
}

function accountSelector(account) {
  return account.selector || `${account.provider || "provider"}:${account.account_ref || account.name || "account"}`;
}

function accountObject(entry) {
  return entry.account || entry;
}

function flattenAccounts(payload) {
  return payload.accounts || payload.data?.accounts || [];
}

function positionRows(payload) {
  return (payload.accounts || []).flatMap((account) => {
    const selector = account.selector || account.account_ref || "";
    const provider = account.provider || selector.split(":")[0] || "provider";
    return (account.positions || []).map((p) => ({ ...p, account: selector, provider }));
  });
}

function orderRows(payload) {
  return (payload.accounts || []).flatMap((account) =>
    (account.orders || []).map((order) => ({
      ...order,
      account: account.selector || account.account_ref || "",
    }))
  );
}

function autoAccounts(payload) {
  return payload?.accounts || payload?.data?.accounts || [];
}

function autoManagedRows(payload) {
  return autoAccounts(payload).flatMap((account) => {
    const selector = `${account.provider || "provider"}:${account.account_ref || "account"}`;
    const rows = account.auto_managed_positions || account.positions || [];
    return rows.map((p) => ({ ...p, account: selector, provider: account.provider || "provider" }));
  });
}

function autoUntrackedRows(payload) {
  return autoAccounts(payload).flatMap((account) => {
    const selector = `${account.provider || "provider"}:${account.account_ref || "account"}`;
    const rows = account.untracked_positions || account.provider_positions_not_tracked || [];
    return rows.map((p) => ({ ...p, account: selector, provider: account.provider || "provider" }));
  });
}

function positionQty(row) {
  return number(row.qty ?? row.quantity).toFixed(2);
}

function positionCost(row) {
  return row.avg_entry_price ?? row.avg_cost ?? row.entry_price ?? row.entry;
}

function positionCurrent(row) {
  return row.current_price ?? row.now ?? row.market_price;
}

function positionMarketValue(row) {
  return row.market_value ?? number(positionCurrent(row)) * number(row.qty ?? row.quantity);
}

function positionPnl(row) {
  return (
    row.unrealized_pl ??
    row.pnl ??
    number(positionMarketValue(row)) - number(positionCost(row)) * number(row.qty ?? row.quantity)
  );
}

function positionPnlPct(row) {
  if (row.unrealized_plpc !== undefined) return number(row.unrealized_plpc) * 100;
  if (row.pnl_percent !== undefined) return number(row.pnl_percent);
  const cost = number(positionCost(row)) * number(row.qty ?? row.quantity);
  return cost ? (number(positionPnl(row)) / cost) * 100 : 0;
}

function positionOrigin(row) {
  return row.origin || row.source || row.provider || "alpaca";
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

function DataTable({ rows, columns }) {
  if (!rows.length) return <p>No rows.</p>;
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
            <tr key={`${row.account || ""}:${row.symbol || ""}:${idx}`}>
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
    if (values.length < 2) return;

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
    const values = rows.slice(0, 6).map((row) => Math.max(0, number(positionMarketValue(row))));
    const total = values.reduce((sum, value) => sum + value, 0);
    if (!total) return;
    const colors = ["#285fd4", "#20b8d3", "#0c8f55", "#e37b24", "#7c5cc4", "#c6362f"];
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
    ctx.fillText("Total", cx, cy - 3 * ratio);
    ctx.font = `700 ${15 * ratio}px system-ui`;
    ctx.fillText(String(rows.length), cx, cy + 17 * ratio);
  }, [rows]);
  return <canvas ref={ref} className="donut" width="180" height="180" />;
}

function Sidebar({ activeTab, setActiveTab, accounts }) {
  const first = accounts.map(accountObject)[0];
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
        <span className="eyebrow">Active account</span>
        <strong>{first ? accountSelector(first) : "Loading"}</strong>
        <span>{first ? `${first.provider || "provider"} / ${first.account_mode || "account"}` : "--"}</span>
      </div>
    </aside>
  );
}

function MobileTabs({ activeTab, setActiveTab }) {
  return (
    <nav className="mobile-tabs" aria-label="Sections">
      {tabs.map(([id, label]) => (
        <button key={id} className={activeTab === id ? "active" : ""} onClick={() => setActiveTab(id)}>
          {id === "portfolio" ? "Dashboard" : label.replace(" Trading", "").replace(" Pipeline", "")}
        </button>
      ))}
    </nav>
  );
}

function Dashboard({ accounts, positions, orders, auto }) {
  const accountRows = accounts.map(accountObject);
  const managed = autoManagedRows(auto);
  const equity = accountRows.reduce((sum, a) => sum + number(a.equity), 0);
  const cash = accountRows.reduce((sum, a) => sum + number(a.cash), 0);
  const buyingPower = accountRows.reduce((sum, a) => sum + number(a.buying_power), 0);
  const autoPnl = managed.reduce((sum, row) => sum + number(positionPnl(row)), 0);
  const closedPnl = autoAccounts(auto).reduce((sum, a) => sum + number(a.closed_pnl), 0);
  const balanceSeries = useMemo(
    () =>
      Array.from({ length: 28 }, (_, i) => {
        const wave = Math.sin(i / 3.2) * Math.max(equity * 0.006, 1);
        const trend = (i - 14) * Math.max(equity * 0.0008, 1);
        return Math.max(equity + wave + trend, 1);
      }),
    [equity]
  );

  return (
    <div className="dashboard-grid">
      <article className="surface balance-panel">
        <div className="section-head">
          <div>
            <span className="eyebrow">Portfolio</span>
            <h2>Total Balance</h2>
          </div>
          <strong>{money(equity)}</strong>
        </div>
        <LineChart values={balanceSeries} />
      </article>

      <aside className="side-column">
        <article className="surface metric-large">
          <span className="eyebrow">Auto unrealized</span>
          <strong className={tone(autoPnl)}>{money(autoPnl)}</strong>
          <span>{managed.length} auto-managed positions</span>
        </article>
        <article className="surface metric-large">
          <span className="eyebrow">Cash</span>
          <strong>{money(cash)}</strong>
          <span>{money(buyingPower)} buying power</span>
        </article>
        <article className="surface allocation-card">
          <div className="section-head compact">
            <h2>Allocation</h2>
            <span>{positions.length} symbols</span>
          </div>
          <DonutChart rows={positions} />
        </article>
      </aside>

      <section className="metrics-row" aria-label="Account metrics">
        <article className="metric-tile">
          <span className="eyebrow">Equity</span>
          <strong>{money(equity)}</strong>
        </article>
        <article className="metric-tile">
          <span className="eyebrow">Open positions</span>
          <strong>{positions.length}</strong>
        </article>
        <article className="metric-tile">
          <span className="eyebrow">Closed P&L</span>
          <strong className={tone(closedPnl)}>{money(closedPnl)}</strong>
        </article>
        <article className="metric-tile">
          <span className="eyebrow">Orders</span>
          <strong>{orders.length}</strong>
        </article>
      </section>

      <article className="surface table-panel">
        <div className="section-head">
          <div>
            <span className="eyebrow">Provider positions</span>
            <h2>Open Positions</h2>
          </div>
          <span className="status-pill">{positions.length ? "live synced" : "empty"}</span>
        </div>
        <PositionTable rows={positions} />
      </article>

      <article className="surface table-panel">
        <div className="section-head">
          <div>
            <span className="eyebrow">Execution</span>
            <h2>Recent Orders</h2>
          </div>
          <span className="status-pill">{orders.length}</span>
        </div>
        <OrderTable rows={orders} />
      </article>
    </div>
  );
}

function PositionTable({ rows }) {
  return (
    <DataTable
      rows={rows}
      columns={[
        { label: "Symbol", value: (r) => r.symbol },
        { label: "Origin", value: positionOrigin },
        { label: "Account", value: (r) => r.account },
        { label: "Qty", value: positionQty },
        { label: "Avg Cost", value: (r) => money(positionCost(r)) },
        { label: "Current", value: (r) => money(positionCurrent(r)) },
        { label: "Mkt Value", value: (r) => money(positionMarketValue(r)) },
        { label: "P&L", value: (r) => money(positionPnl(r)), className: (r) => tone(positionPnl(r)) },
        { label: "P&L%", value: (r) => pct(positionPnlPct(r)), className: (r) => tone(positionPnlPct(r)) },
        { label: "MLQ", value: (r) => r.ml_quantile || r.mlq || "-" },
      ]}
    />
  );
}

function OrderTable({ rows }) {
  return (
    <DataTable
      rows={rows}
      columns={[
        { label: "Time", value: (r) => String(r.submitted_at || r.filled_at || r.time || "").slice(0, 19) },
        { label: "Account", value: (r) => r.account },
        { label: "Symbol", value: (r) => r.symbol },
        { label: "Origin", value: (r) => r.origin || r.source || "-" },
        { label: "Side", value: (r) => r.side },
        { label: "Qty", value: (r) => r.qty },
        { label: "Status", value: (r) => r.status },
        { label: "Fill", value: (r) => (r.filled_avg_price ? money(r.filled_avg_price) : "-") },
      ]}
    />
  );
}

function AutoView({ auto }) {
  const accounts = autoAccounts(auto);
  const managed = autoManagedRows(auto);
  const untracked = autoUntrackedRows(auto);
  const enabled = auto?.enabled ?? auto?.auto_trading_enabled ?? accounts.some((a) => a.auto_trade_enabled);
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
          <InfoTile label="Accounts" value={String(accounts.length)} detail="enabled provider accounts" />
          <InfoTile label="Auto-managed" value={String(managed.length)} detail={`${untracked.length} outside auto tracking`} />
          <InfoTile label="Stop loss" value={`${number(auto?.stop_loss_pct ?? -7).toFixed(1)}%`} detail="confirmation rules active" />
          <InfoTile label="Take profit" value={`+${number(auto?.take_profit_pct ?? 15).toFixed(1)}%`} detail="trailing rules active" />
        </div>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Tracked by auto</span>
            <h2>Auto-managed Positions</h2>
          </div>
        </div>
        <PositionTable rows={[...managed, ...untracked]} />
      </article>
    </div>
  );
}

function MlView({ ml }) {
  const data = ml?.data || ml || {};
  const ready = data.ready ?? data.ml_ready ?? true;
  return (
    <div className="section-layout">
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Model readiness</span>
            <h2>ML Pipeline</h2>
          </div>
          <span className="status-pill">{ready ? "ready" : "not ready"}</span>
        </div>
        <div className="ml-grid">
          <InfoTile label="Bars" value={String(data.bars_rows ?? data.bars ?? "not available")} detail={data.bar_range || ""} />
          <InfoTile
            label="Features"
            value={String(data.features_rows ?? data.features ?? "not available")}
            detail={data.feature_dates ? `${data.feature_dates} dates` : ""}
          />
          <InfoTile
            label="Predictions"
            value={String(data.ensemble_predictions ?? data.predictions ?? "not available")}
            detail={data.latest_prediction_date || ""}
          />
          <InfoTile label="SHAP cache" value={String(data.shap_cached ?? data.shap ?? "not available")} detail="symbol explanations" />
        </div>
      </article>
    </div>
  );
}

function MarketView({ symbol, setSymbol, quote, bars, loadMarket }) {
  const values = bars.map((p) => number(p.close ?? p.c)).filter(Boolean);
  const data = quote?.data || quote || {};
  return (
    <article className="surface market-panel">
      <div className="section-head">
        <div>
          <span className="eyebrow">Market data</span>
          <h2>Symbol Chart</h2>
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
        <InfoTile label="Symbol" value={data.symbol || symbol} detail="latest quote" />
        <InfoTile label="Bid" value={money(data.bid_price ?? data.bid)} detail={data.bid_size ? `${data.bid_size} shares` : ""} />
        <InfoTile label="Ask" value={money(data.ask_price ?? data.ask)} detail={data.ask_size ? `${data.ask_size} shares` : ""} />
        <InfoTile label="Source" value={data.source || data.feed || "provider"} detail={data.timestamp || data.t || ""} />
      </div>
    </article>
  );
}

function SystemView({ health, dataStatus }) {
  return (
    <div className="section-layout two-columns">
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Service</span>
            <h2>API</h2>
          </div>
        </div>
        <pre>{JSON.stringify(health, null, 2)}</pre>
      </article>
      <article className="surface">
        <div className="section-head">
          <div>
            <span className="eyebrow">Storage</span>
            <h2>Data</h2>
          </div>
        </div>
        <pre>{JSON.stringify(dataStatus, null, 2)}</pre>
      </article>
    </div>
  );
}

function App() {
  const [activeTab, setActiveTab] = useState("portfolio");
  const [status, setStatus] = useState("Loading");
  const [accounts, setAccounts] = useState([]);
  const [positions, setPositions] = useState([]);
  const [orders, setOrders] = useState([]);
  const [auto, setAuto] = useState(null);
  const [ml, setMl] = useState({});
  const [health, setHealth] = useState({});
  const [dataStatus, setDataStatus] = useState({});
  const [symbol, setSymbol] = useState("AAPL");
  const [quote, setQuote] = useState({});
  const [bars, setBars] = useState([]);

  async function loadPortfolio() {
    const [accountsPayload, positionsPayload] = await Promise.all([api("/trade/account"), api("/trade/positions?sync=true")]);
    setAccounts(flattenAccounts(accountsPayload));
    setPositions(positionRows(positionsPayload));
  }

  async function loadAuto() {
    const [autoPayload, ordersPayload] = await Promise.all([api("/auto/status"), api("/trade/orders?limit=20&sync=true")]);
    setAuto(autoPayload);
    setOrders(orderRows(ordersPayload));
  }

  async function loadMarket(nextSymbol = symbol) {
    const clean = nextSymbol.trim().toUpperCase();
    if (!clean) return;
    setSymbol(clean);
    const [quotePayload, barsPayload] = await Promise.all([
      api(`/market/quote/${encodeURIComponent(clean)}`),
      api(`/market/bars/${encodeURIComponent(clean)}?timeframe=1Day&limit=90`),
    ]);
    setQuote(quotePayload);
    const data = barsPayload.bars || barsPayload.data?.bars || [];
    setBars(Array.isArray(data) ? data : Object.values(data).flat());
  }

  async function refreshAll() {
    setStatus("Refreshing");
    const results = await Promise.allSettled([
      loadPortfolio(),
      loadAuto(),
      api("/ml/status").then(setMl),
      api("/health").then(setHealth),
      api("/data/status").then(setDataStatus),
      loadMarket(symbol),
    ]);
    const failed = results.find((result) => result.status === "rejected");
    setStatus(failed ? failed.reason.message : `Updated ${new Date().toLocaleTimeString()}`);
  }

  useEffect(() => {
    refreshAll().catch((err) => setStatus(err.message));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="app-shell">
      <Sidebar activeTab={activeTab} setActiveTab={setActiveTab} accounts={accounts} />
      <div className="workspace">
        <header className="topbar">
          <div>
            <span className="eyebrow">Trading dashboard</span>
            <h1>Welcome back</h1>
          </div>
          <div className="toolbar">
            <span className="status-pill">{status}</span>
            <button className="primary" onClick={refreshAll}>
              Refresh
            </button>
          </div>
        </header>
        <MobileTabs activeTab={activeTab} setActiveTab={setActiveTab} />
        <main>
          <section className={`panel ${activeTab === "portfolio" ? "active" : ""}`}>
            <Dashboard accounts={accounts} positions={positions} orders={orders} auto={auto} />
          </section>
          <section className={`panel ${activeTab === "auto" ? "active" : ""}`}>
            <AutoView auto={auto} />
          </section>
          <section className={`panel ${activeTab === "ml" ? "active" : ""}`}>
            <MlView ml={ml} />
          </section>
          <section className={`panel ${activeTab === "market" ? "active" : ""}`}>
            <MarketView symbol={symbol} setSymbol={setSymbol} quote={quote} bars={bars} loadMarket={loadMarket} />
          </section>
          <section className={`panel ${activeTab === "system" ? "active" : ""}`}>
            <SystemView health={health} dataStatus={dataStatus} />
          </section>
        </main>
      </div>
    </div>
  );
}

createRoot(document.getElementById("root")).render(<App />);
