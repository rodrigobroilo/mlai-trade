use crate::{config, paths};
use chrono::{Datelike, NaiveDate, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum FilingStatus {
    Single,
    MarriedFilingJointly,
    MarriedFilingSeparately,
    HeadOfHousehold,
}

impl FilingStatus {
    fn parse(value: &str) -> anyhow::Result<Self> {
        let normalized = value
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-'], "_")
            .replace("filling", "filing");
        match normalized.as_str() {
            "single" => Ok(Self::Single),
            "married_filing_jointly"
            | "married_jointly"
            | "mfj"
            | "qualifying_surviving_spouse"
            | "qualifying_survive_spouse"
            | "qss" => Ok(Self::MarriedFilingJointly),
            "married_filing_separately" | "married_separately" | "mfs" => {
                Ok(Self::MarriedFilingSeparately)
            }
            "head_of_household" | "hoh" => Ok(Self::HeadOfHousehold),
            _ => anyhow::bail!(
                "Unsupported tax.filing_status='{}'. Use single, married_filing_jointly, married_filing_separately, or head_of_household.",
                value
            ),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Single => "Single",
            Self::MarriedFilingJointly => "Married filing jointly / qualifying surviving spouse",
            Self::MarriedFilingSeparately => "Married filing separately",
            Self::HeadOfHousehold => "Head of household",
        }
    }

    fn config_key(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::MarriedFilingJointly => "married_filing_jointly",
            Self::MarriedFilingSeparately => "married_filing_separately",
            Self::HeadOfHousehold => "head_of_household",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
struct OrdinaryBracket {
    upper: Option<f64>,
    rate: f64,
}

type TaxBracket = OrdinaryBracket;

#[derive(Debug, Clone, Deserialize)]
struct TaxBracketFile {
    years: BTreeMap<String, TaxYearConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct TaxYearConfig {
    ordinary_income: FilingStatusBrackets,
    long_term_capital_gains: FilingStatusBrackets,
    net_investment_income_tax: NiitConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct FilingStatusBrackets {
    single: Vec<TaxBracket>,
    married_filing_jointly: Vec<TaxBracket>,
    married_filing_separately: Vec<TaxBracket>,
    head_of_household: Vec<TaxBracket>,
}

impl FilingStatusBrackets {
    fn for_status(&self, status: FilingStatus) -> &[TaxBracket] {
        match status {
            FilingStatus::Single => &self.single,
            FilingStatus::MarriedFilingJointly => &self.married_filing_jointly,
            FilingStatus::MarriedFilingSeparately => &self.married_filing_separately,
            FilingStatus::HeadOfHousehold => &self.head_of_household,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct NiitConfig {
    rate: f64,
    thresholds: FilingStatusThresholds,
}

#[derive(Debug, Clone, Deserialize)]
struct FilingStatusThresholds {
    single: f64,
    married_filing_jointly: f64,
    married_filing_separately: f64,
    head_of_household: f64,
}

impl FilingStatusThresholds {
    fn for_status(&self, status: FilingStatus) -> f64 {
        match status {
            FilingStatus::Single => self.single,
            FilingStatus::MarriedFilingJointly => self.married_filing_jointly,
            FilingStatus::MarriedFilingSeparately => self.married_filing_separately,
            FilingStatus::HeadOfHousehold => self.head_of_household,
        }
    }
}

#[derive(Debug, Clone)]
struct TaxYearTable {
    year: i32,
    ordinary: Vec<TaxBracket>,
    capital_gains: Vec<TaxBracket>,
    niit_rate: f64,
    niit_threshold: f64,
    source_path: PathBuf,
}

#[derive(Debug, Clone)]
struct ClosedPosition {
    provider: String,
    account_ref: String,
    account_mode: String,
    paper_account: bool,
    symbol: String,
    qty: f64,
    entry_date: NaiveDate,
    entry_price: f64,
    exit_date: NaiveDate,
    exit_price: f64,
    pnl: f64,
    source: String,
}

#[derive(Debug)]
struct FillActivity {
    provider: String,
    account_ref: String,
    account_mode: String,
    paper_account: bool,
    symbol: String,
    side: String,
    qty: f64,
    price: f64,
    date: NaiveDate,
}

#[derive(Debug, Clone)]
struct OpenLot {
    date: NaiveDate,
    qty: f64,
    price: f64,
}

#[derive(Debug, Default, Clone, Serialize)]
struct TermTotals {
    gains: f64,
    losses: f64,
    net: f64,
    count: usize,
}

#[derive(Debug, Default, Clone, Serialize)]
struct TaxableNetting {
    taxable_short_term: f64,
    taxable_long_term: f64,
    capital_loss_after_netting: f64,
}

#[derive(Debug, Clone, Serialize)]
struct RateSummary {
    ordinary_marginal_rate: f64,
    short_term_effective_rate: f64,
    long_term_effective_rate: f64,
    niit_rate: f64,
    short_term_with_niit_effective_rate: f64,
    long_term_with_niit_effective_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
struct FederalTaxEstimate {
    short_term: f64,
    long_term: f64,
    net_investment_income_tax: f64,
    total: f64,
}

#[derive(Debug, Clone, Serialize)]
struct TaxEstimate {
    year: i32,
    quarter: u8,
    period_label: String,
    period_start: String,
    period_end: String,
    scope: String,
    provider: String,
    account_ref: String,
    account_mode: String,
    paper_account: i64,
    filing_status: FilingStatus,
    filing_status_label: String,
    estimated_annual_income: f64,
    include_paper_accounts_for_estimate: bool,
    excluded_paper_positions: usize,
    short_term: TermTotals,
    long_term: TermTotals,
    total_net: f64,
    taxable_after_netting: TaxableNetting,
    rates: RateSummary,
    estimated_federal_tax: FederalTaxEstimate,
    position_count: usize,
    generated_at_utc: String,
}

fn tax_table(year: i32, status: FilingStatus) -> anyhow::Result<TaxYearTable> {
    let path = config::tax_brackets_path();
    let content = fs::read_to_string(&path).map_err(|err| {
        anyhow::anyhow!(
            "unable to read tax bracket config {}: {}. Copy config/tax-brackets.example.json to {} or set tax.brackets_file in {}.",
            path.display(),
            err,
            path.display(),
            config::config_path().display()
        )
    })?;
    let file = serde_json::from_str::<TaxBracketFile>(&content)
        .map_err(|err| anyhow::anyhow!("invalid tax bracket config {}: {}", path.display(), err))?;
    let key = year.to_string();
    let year_config = file.years.get(&key).ok_or_else(|| {
        let supported = file.years.keys().cloned().collect::<Vec<_>>().join(", ");
        anyhow::anyhow!(
            "No IRS tax bracket table for tax year {} in {}. Supported years: {}.",
            year,
            path.display(),
            if supported.is_empty() {
                "none".into()
            } else {
                supported
            }
        )
    })?;
    let ordinary = year_config.ordinary_income.for_status(status).to_vec();
    let capital_gains = year_config
        .long_term_capital_gains
        .for_status(status)
        .to_vec();
    validate_brackets(year, status, "ordinary_income", &ordinary)?;
    validate_brackets(year, status, "long_term_capital_gains", &capital_gains)?;
    Ok(TaxYearTable {
        year,
        ordinary,
        capital_gains,
        niit_rate: year_config.net_investment_income_tax.rate,
        niit_threshold: year_config
            .net_investment_income_tax
            .thresholds
            .for_status(status),
        source_path: path,
    })
}

fn validate_brackets(
    year: i32,
    status: FilingStatus,
    name: &str,
    brackets: &[TaxBracket],
) -> anyhow::Result<()> {
    if brackets.is_empty() {
        anyhow::bail!(
            "tax bracket config year {} {} {} is empty.",
            year,
            status.config_key(),
            name
        );
    }
    let mut previous = 0.0;
    for (idx, bracket) in brackets.iter().enumerate() {
        if !(0.0..=1.0).contains(&bracket.rate) {
            anyhow::bail!(
                "tax bracket config year {} {} {} row {} has invalid rate {}.",
                year,
                status.config_key(),
                name,
                idx + 1,
                bracket.rate
            );
        }
        if let Some(upper) = bracket.upper {
            if upper <= previous {
                anyhow::bail!(
                    "tax bracket config year {} {} {} row {} upper {} must be greater than previous upper {}.",
                    year,
                    status.config_key(),
                    name,
                    idx + 1,
                    upper,
                    previous
                );
            }
            previous = upper;
        } else if idx + 1 != brackets.len() {
            anyhow::bail!(
                "tax bracket config year {} {} {} can only use upper=null on the final row.",
                year,
                status.config_key(),
                name
            );
        }
    }
    if brackets.last().and_then(|bracket| bracket.upper).is_some() {
        anyhow::bail!(
            "tax bracket config year {} {} {} must end with upper=null.",
            year,
            status.config_key(),
            name
        );
    }
    Ok(())
}

fn quarter_range(year: i32, quarter: u8) -> anyhow::Result<(NaiveDate, NaiveDate)> {
    let (start_month, end_month, end_day) = match quarter {
        1 => (1, 3, 31),
        2 => (4, 6, 30),
        3 => (7, 9, 30),
        4 => (10, 12, 31),
        _ => anyhow::bail!("quarter must be 1, 2, 3, or 4"),
    };
    Ok((
        NaiveDate::from_ymd_opt(year, start_month, 1)
            .ok_or_else(|| anyhow::anyhow!("invalid year {}", year))?,
        NaiveDate::from_ymd_opt(year, end_month, end_day)
            .ok_or_else(|| anyhow::anyhow!("invalid year {}", year))?,
    ))
}

fn parse_quarters(value: Option<String>) -> anyhow::Result<Vec<u8>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("all") || value.eq_ignore_ascii_case("ytd") {
        return Ok(Vec::new());
    }
    let mut quarters = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start = start.trim().parse::<u8>()?;
            let end = end.trim().parse::<u8>()?;
            if start > end {
                anyhow::bail!("invalid --quarter range '{}': start is after end", part);
            }
            for quarter in start..=end {
                quarters.push(quarter);
            }
        } else {
            quarters.push(part.parse::<u8>()?);
        }
    }
    quarters.sort_unstable();
    quarters.dedup();
    if quarters.iter().any(|quarter| !(1..=4).contains(quarter)) {
        anyhow::bail!("--quarter must contain only values 1, 2, 3, or 4");
    }
    if quarters.windows(2).any(|pair| pair[1] != pair[0] + 1) {
        anyhow::bail!("--quarter must be contiguous, for example 1,2 or 1-4");
    }
    Ok(quarters)
}

fn normalize_account_filters(filters: &[String]) -> Vec<String> {
    filters
        .iter()
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn account_filters_may_include_paper(tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    tokens.iter().any(|token| {
        if matches!(token.as_str(), "paper" | "alpaca:paper") {
            return true;
        }
        if matches!(
            token.as_str(),
            "all" | "real" | "live" | "individual" | "brokerage"
        ) {
            return false;
        }
        config::alpaca_accounts()
            .ok()
            .map(|accounts| {
                accounts.iter().any(|account| {
                    account.is_paper()
                        && (token == &account.account_ref().to_ascii_lowercase()
                            || token
                                == &format!(
                                    "{}:{}",
                                    account.provider(),
                                    account.account_ref().to_ascii_lowercase()
                                ))
                })
            })
            .unwrap_or(true)
    })
}

fn position_matches_account_filters(position: &ClosedPosition, tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return true;
    }
    let provider = position.provider.to_ascii_lowercase();
    let account_ref = position.account_ref.to_ascii_lowercase();
    let account_mode = position.account_mode.to_ascii_lowercase();
    tokens.iter().any(|token| {
        token == "all"
            || token == &account_ref
            || token == &format!("{provider}:{account_ref}")
            || token == &format!("{provider}:{account_mode}")
            || (token == "paper" && position.paper_account)
            || (matches!(token.as_str(), "real" | "live" | "individual" | "brokerage")
                && !position.paper_account)
    })
}

fn tax_period_range(
    year: i32,
    quarters: &[u8],
) -> anyhow::Result<(NaiveDate, NaiveDate, u8, String)> {
    let current_year = Utc::now().year();
    let today = Utc::now().date_naive();
    let (start, mut end, quarter_id, label) = if quarters.is_empty() {
        (
            NaiveDate::from_ymd_opt(year, 1, 1)
                .ok_or_else(|| anyhow::anyhow!("invalid year {}", year))?,
            NaiveDate::from_ymd_opt(year, 12, 31)
                .ok_or_else(|| anyhow::anyhow!("invalid year {}", year))?,
            0,
            "year_to_date".to_string(),
        )
    } else {
        let first = *quarters.first().unwrap();
        let last = *quarters.last().unwrap();
        let (start, _) = quarter_range(year, first)?;
        let (_, end) = quarter_range(year, last)?;
        let quarter_id = if first == last {
            first
        } else {
            first * 10 + last
        };
        let label = if first == last {
            format!("q{}", first)
        } else {
            format!("q{}_q{}", first, last)
        };
        (start, end, quarter_id, label)
    };
    if year == current_year {
        end = end.min(today);
    }
    if end < start {
        anyhow::bail!(
            "selected tax period {} starts in the future for current year {}",
            label,
            year
        );
    }
    Ok((start, end, quarter_id, label))
}

fn open_db() -> anyhow::Result<Connection> {
    let _ = paths::ensure_state_dir()?;
    let conn = Connection::open(paths::scanner_db_path())?;
    Ok(conn)
}

fn tax_db_path() -> PathBuf {
    paths::db_dir().join("tax.db")
}

fn open_tax_db() -> anyhow::Result<Connection> {
    paths::ensure_runtime_dirs()?;
    let conn = Connection::open(tax_db_path())?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS tax_estimates (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            year INTEGER NOT NULL,
            quarter INTEGER NOT NULL,
            period_label TEXT NOT NULL DEFAULT '',
            period_start TEXT NOT NULL,
            period_end TEXT NOT NULL,
            scope TEXT NOT NULL,
            provider TEXT NOT NULL,
            account_ref TEXT NOT NULL,
            account_mode TEXT NOT NULL,
            paper_account INTEGER NOT NULL,
            filing_status TEXT NOT NULL,
            filing_status_label TEXT NOT NULL,
            estimated_annual_income REAL NOT NULL,
            include_paper_accounts_for_estimate INTEGER NOT NULL,
            excluded_paper_positions INTEGER NOT NULL,
            short_gains REAL NOT NULL,
            short_losses REAL NOT NULL,
            short_net REAL NOT NULL,
            short_count INTEGER NOT NULL,
            long_gains REAL NOT NULL,
            long_losses REAL NOT NULL,
            long_net REAL NOT NULL,
            long_count INTEGER NOT NULL,
            total_net REAL NOT NULL,
            taxable_short_term REAL NOT NULL,
            taxable_long_term REAL NOT NULL,
            capital_loss_after_netting REAL NOT NULL,
            ordinary_marginal_rate REAL NOT NULL,
            short_term_effective_rate REAL NOT NULL,
            long_term_effective_rate REAL NOT NULL,
            niit_rate REAL NOT NULL DEFAULT 0.0,
            short_term_with_niit_effective_rate REAL NOT NULL DEFAULT 0.0,
            long_term_with_niit_effective_rate REAL NOT NULL DEFAULT 0.0,
            estimated_short_term_tax REAL NOT NULL,
            estimated_long_term_tax REAL NOT NULL,
            estimated_niit_tax REAL NOT NULL DEFAULT 0.0,
            estimated_total_tax REAL NOT NULL,
            position_count INTEGER NOT NULL,
            generated_at_utc TEXT NOT NULL,
            UNIQUE(year, quarter, scope, provider, account_ref, account_mode, paper_account)
         );
         CREATE INDEX IF NOT EXISTS idx_tax_estimates_period
           ON tax_estimates(year, quarter, scope, provider, account_ref);",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "period_label",
        "period_label TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "niit_rate",
        "niit_rate REAL NOT NULL DEFAULT 0.0",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "short_term_with_niit_effective_rate",
        "short_term_with_niit_effective_rate REAL NOT NULL DEFAULT 0.0",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "long_term_with_niit_effective_rate",
        "long_term_with_niit_effective_rate REAL NOT NULL DEFAULT 0.0",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "estimated_niit_tax",
        "estimated_niit_tax REAL NOT NULL DEFAULT 0.0",
    )?;
    Ok(conn)
}

fn tax_table_columns(conn: &Connection, table: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|row| row.ok())
        .collect();
    Ok(columns)
}

fn ensure_tax_column(
    conn: &Connection,
    table: &str,
    column: &str,
    ddl: &str,
) -> anyhow::Result<()> {
    if !tax_table_columns(conn, table)?
        .iter()
        .any(|name| name == column)
    {
        conn.execute_batch(&format!("ALTER TABLE {} ADD COLUMN {}", table, ddl))?;
    }
    Ok(())
}

fn save_estimates(estimates: &[TaxEstimate]) -> anyhow::Result<()> {
    let conn = open_tax_db()?;
    for estimate in estimates {
        conn.execute(
            "INSERT OR REPLACE INTO tax_estimates (
                year, quarter, period_label, period_start, period_end, scope, provider, account_ref,
                account_mode, paper_account, filing_status, filing_status_label,
                estimated_annual_income, include_paper_accounts_for_estimate,
                excluded_paper_positions, short_gains, short_losses, short_net, short_count,
                long_gains, long_losses, long_net, long_count, total_net, taxable_short_term,
                taxable_long_term, capital_loss_after_netting, ordinary_marginal_rate,
                short_term_effective_rate, long_term_effective_rate, niit_rate,
                short_term_with_niit_effective_rate, long_term_with_niit_effective_rate,
                estimated_short_term_tax, estimated_long_term_tax, estimated_niit_tax,
                estimated_total_tax, position_count, generated_at_utc
             )
             VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39
             )",
            params![
                estimate.year,
                estimate.quarter,
                estimate.period_label,
                estimate.period_start,
                estimate.period_end,
                estimate.scope,
                estimate.provider,
                estimate.account_ref,
                estimate.account_mode,
                estimate.paper_account,
                format!("{:?}", estimate.filing_status),
                estimate.filing_status_label,
                estimate.estimated_annual_income,
                if estimate.include_paper_accounts_for_estimate {
                    1
                } else {
                    0
                },
                estimate.excluded_paper_positions as i64,
                estimate.short_term.gains,
                estimate.short_term.losses,
                estimate.short_term.net,
                estimate.short_term.count as i64,
                estimate.long_term.gains,
                estimate.long_term.losses,
                estimate.long_term.net,
                estimate.long_term.count as i64,
                estimate.total_net,
                estimate.taxable_after_netting.taxable_short_term,
                estimate.taxable_after_netting.taxable_long_term,
                estimate.taxable_after_netting.capital_loss_after_netting,
                estimate.rates.ordinary_marginal_rate,
                estimate.rates.short_term_effective_rate,
                estimate.rates.long_term_effective_rate,
                estimate.rates.niit_rate,
                estimate.rates.short_term_with_niit_effective_rate,
                estimate.rates.long_term_with_niit_effective_rate,
                estimate.estimated_federal_tax.short_term,
                estimate.estimated_federal_tax.long_term,
                estimate.estimated_federal_tax.net_investment_income_tax,
                estimate.estimated_federal_tax.total,
                estimate.position_count as i64,
                estimate.generated_at_utc,
            ],
        )?;
    }
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        params![table],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

fn load_closed_positions(
    conn: &Connection,
    start: NaiveDate,
    end: NaiveDate,
    include_paper: bool,
) -> anyhow::Result<(Vec<ClosedPosition>, usize)> {
    let mut positions = Vec::new();
    let mut excluded_paper = 0usize;

    if !table_exists(conn, "auto_positions")? {
        return load_provider_fill_positions(conn, start, end, include_paper);
    }

    let start_str = start.format("%Y-%m-%d").to_string();
    let end_str = end.format("%Y-%m-%d").to_string();
    let mut stmt = conn.prepare(
        "SELECT provider, account_ref, account_mode, paper_account,
                entry_date, exit_date, COALESCE(pnl, 0.0), symbol,
                CAST(COALESCE(shares, 0) AS REAL), COALESCE(entry_price, 0.0), COALESCE(exit_price, 0.0)
         FROM auto_positions
         WHERE status='closed'
           AND exit_date >= ?1 AND exit_date <= ?2
         ORDER BY exit_date, provider, account_ref",
    )?;
    let mut rows = stmt.query(params![start_str, end_str])?;
    while let Some(row) = rows.next()? {
        let paper_account = row.get::<_, i64>(3).unwrap_or(1) != 0;
        if paper_account && !include_paper {
            excluded_paper += 1;
            continue;
        }
        let Some(exit_date) = row.get::<_, Option<String>>(5)? else {
            continue;
        };
        positions.push(ClosedPosition {
            provider: row.get(0)?,
            account_ref: row.get(1)?,
            account_mode: row.get(2)?,
            paper_account,
            symbol: row.get::<_, String>(7)?.to_ascii_uppercase(),
            qty: row.get::<_, f64>(8)?,
            entry_date: NaiveDate::parse_from_str(&row.get::<_, String>(4)?, "%Y-%m-%d")?,
            entry_price: row.get(9)?,
            exit_date: NaiveDate::parse_from_str(&exit_date, "%Y-%m-%d")?,
            exit_price: row.get(10)?,
            pnl: row.get(6)?,
            source: "auto_positions".to_string(),
        });
    }

    let (provider_positions, provider_excluded_paper) =
        load_provider_fill_positions(conn, start, end, include_paper)?;
    positions.extend(provider_positions);
    excluded_paper += provider_excluded_paper;

    Ok((positions, excluded_paper))
}

fn load_provider_fill_positions(
    conn: &Connection,
    start: NaiveDate,
    end: NaiveDate,
    include_paper: bool,
) -> anyhow::Result<(Vec<ClosedPosition>, usize)> {
    if !table_exists(conn, "provider_fill_activities")? {
        return Ok((Vec::new(), 0));
    }
    let end_str = end.format("%Y-%m-%d").to_string();
    let mut stmt = conn.prepare(
        "SELECT provider, account_ref, account_mode, paper_account, symbol, side,
                COALESCE(qty, 0.0), COALESCE(price, 0.0), transaction_time
         FROM provider_fill_activities
         WHERE transaction_time IS NOT NULL
           AND substr(transaction_time, 1, 10) <= ?1
           AND NOT EXISTS (
                SELECT 1 FROM auto_trades t
                WHERE t.provider = provider_fill_activities.provider
                  AND t.account_ref = provider_fill_activities.account_ref
                  AND t.paper_account = provider_fill_activities.paper_account
                  AND t.order_id = provider_fill_activities.order_id
           )
         ORDER BY provider, account_ref, paper_account, transaction_time, activity_id",
    )?;
    let fills = stmt
        .query_map(params![end_str], |row| {
            let ts: String = row.get(8)?;
            let date_part = ts.get(0..10).unwrap_or("");
            let date = NaiveDate::parse_from_str(date_part, "%Y-%m-%d").map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?;
            Ok(FillActivity {
                provider: row.get(0)?,
                account_ref: row.get(1)?,
                account_mode: row.get(2)?,
                paper_account: row.get::<_, i64>(3).unwrap_or(1) != 0,
                symbol: row.get::<_, String>(4)?.to_ascii_uppercase(),
                side: row.get::<_, String>(5)?.to_ascii_lowercase(),
                qty: row.get(6)?,
                price: row.get(7)?,
                date,
            })
        })?
        .filter_map(|row| row.ok())
        .filter(|fill| fill.qty > 0.0 && fill.price > 0.0)
        .collect::<Vec<_>>();

    let mut lots: BTreeMap<(String, String, i64, String), VecDeque<OpenLot>> = BTreeMap::new();
    let mut positions = Vec::new();
    let mut excluded_paper = 0usize;

    for fill in fills {
        let key = (
            fill.provider.clone(),
            fill.account_ref.clone(),
            if fill.paper_account { 1 } else { 0 },
            fill.symbol.clone(),
        );
        if fill.side == "buy" {
            lots.entry(key).or_default().push_back(OpenLot {
                date: fill.date,
                qty: fill.qty,
                price: fill.price,
            });
            continue;
        }
        if fill.side != "sell" {
            continue;
        }

        let mut remaining = fill.qty;
        let account_lots = lots.entry(key).or_default();
        while remaining > 0.0000001 {
            let Some(mut lot) = account_lots.pop_front() else {
                break;
            };
            let matched_qty = remaining.min(lot.qty);
            remaining -= matched_qty;
            lot.qty -= matched_qty;
            if lot.qty > 0.0000001 {
                account_lots.push_front(lot.clone());
            }

            if fill.date < start || fill.date > end {
                continue;
            }
            if fill.paper_account && !include_paper {
                excluded_paper += 1;
                continue;
            }
            positions.push(ClosedPosition {
                provider: fill.provider.clone(),
                account_ref: fill.account_ref.clone(),
                account_mode: fill.account_mode.clone(),
                paper_account: fill.paper_account,
                symbol: fill.symbol.clone(),
                qty: matched_qty,
                entry_date: lot.date,
                entry_price: lot.price,
                exit_date: fill.date,
                exit_price: fill.price,
                pnl: (fill.price - lot.price) * matched_qty,
                source: "provider_fill_activities".to_string(),
            });
        }
    }

    Ok((positions, excluded_paper))
}

fn add_to_totals(totals: &mut TermTotals, pnl: f64) {
    if pnl >= 0.0 {
        totals.gains += pnl;
    } else {
        totals.losses += pnl;
    }
    totals.net += pnl;
    totals.count += 1;
}

fn net_taxable(short_net: f64, long_net: f64) -> TaxableNetting {
    if short_net >= 0.0 && long_net >= 0.0 {
        return TaxableNetting {
            taxable_short_term: short_net,
            taxable_long_term: long_net,
            capital_loss_after_netting: 0.0,
        };
    }
    if short_net >= 0.0 && long_net < 0.0 {
        let net = short_net + long_net;
        return TaxableNetting {
            taxable_short_term: net.max(0.0),
            taxable_long_term: 0.0,
            capital_loss_after_netting: net.min(0.0),
        };
    }
    if short_net < 0.0 && long_net >= 0.0 {
        let net = long_net + short_net;
        return TaxableNetting {
            taxable_short_term: 0.0,
            taxable_long_term: net.max(0.0),
            capital_loss_after_netting: net.min(0.0),
        };
    }
    TaxableNetting {
        taxable_short_term: 0.0,
        taxable_long_term: 0.0,
        capital_loss_after_netting: short_net + long_net,
    }
}

fn progressive_tax(income: f64, brackets: &[OrdinaryBracket]) -> f64 {
    if income <= 0.0 {
        return 0.0;
    }
    let mut prev = 0.0;
    let mut tax = 0.0;
    for bracket in brackets {
        let upper = bracket.upper.unwrap_or(f64::INFINITY);
        let amount = income.min(upper) - prev;
        if amount > 0.0 {
            tax += amount * bracket.rate;
        }
        if income <= upper {
            break;
        }
        prev = upper;
    }
    tax
}

fn marginal_rate(income: f64, brackets: &[OrdinaryBracket]) -> f64 {
    let income = income.max(0.0);
    for bracket in brackets {
        if bracket.upper.map(|upper| income <= upper).unwrap_or(true) {
            return bracket.rate;
        }
    }
    brackets.last().map(|bracket| bracket.rate).unwrap_or(0.0)
}

fn niit_tax_for_trading_gains(
    table: &TaxYearTable,
    estimated_income: f64,
    taxable_short_term: f64,
    taxable_long_term: f64,
) -> f64 {
    let net_investment_income = (taxable_short_term + taxable_long_term).max(0.0);
    if net_investment_income <= 0.0 {
        return 0.0;
    }
    let modified_agi_excess =
        (estimated_income + net_investment_income - table.niit_threshold).max(0.0);
    net_investment_income.min(modified_agi_excess) * table.niit_rate
}

fn capital_gain_marginal_rate(base_income: f64, brackets: &[TaxBracket]) -> f64 {
    marginal_rate(base_income, brackets)
}

fn capital_gain_tax(base_income: f64, gain: f64, brackets: &[TaxBracket]) -> f64 {
    if gain <= 0.0 {
        return 0.0;
    }
    progressive_tax(base_income.max(0.0) + gain, brackets) - progressive_tax(base_income, brackets)
}

fn money(value: f64) -> String {
    if value < 0.0 {
        format!("-${:.2}", value.abs())
    } else {
        format!("${:.2}", value)
    }
}

fn pct(rate: f64) -> String {
    format!("{:.1}%", rate * 100.0)
}

fn term_for_position(position: &ClosedPosition) -> &'static str {
    if (position.exit_date - position.entry_date).num_days() > 365 {
        "long"
    } else {
        "short"
    }
}

fn operation_tax_impact(
    position: &ClosedPosition,
    table: &TaxYearTable,
    estimated_income: f64,
) -> f64 {
    let base_rate = if term_for_position(position) == "long" {
        capital_gain_marginal_rate(estimated_income, &table.capital_gains)
    } else {
        marginal_rate(estimated_income, &table.ordinary)
    };
    let niit_rate = if estimated_income > table.niit_threshold {
        table.niit_rate
    } else {
        0.0
    };
    position.pnl * (base_rate + niit_rate)
}

fn quarter_breakdown(year: i32, account_filters: &[String]) -> anyhow::Result<Vec<TaxEstimate>> {
    let mut estimates = Vec::new();
    for quarter in refreshable_quarters_for_date(year, Utc::now().date_naive()) {
        let (estimate, _, _, _, _, _, _, _) =
            build_estimates_with_filters(year, &[quarter], account_filters)?;
        estimates.push(estimate);
    }
    Ok(estimates)
}

fn refreshable_quarters_for_date(year: i32, today: NaiveDate) -> Vec<u8> {
    (1..=4)
        .filter(|quarter| {
            quarter_range(year, *quarter)
                .map(|(start, _)| year < today.year() || start <= today)
                .unwrap_or(false)
        })
        .collect()
}

fn known_tax_accounts() -> anyhow::Result<Vec<serde_json::Value>> {
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();

    if let Ok(accounts) = config::alpaca_accounts() {
        for account in accounts {
            let key = format!(
                "{}|{}|{}|{}",
                account.provider(),
                account.account_ref(),
                account.account_mode,
                account.is_paper()
            );
            if seen.insert(key) {
                rows.push(serde_json::json!({
                    "provider": account.provider(),
                    "account_ref": account.account_ref(),
                    "selector": format!("{}:{}", account.provider(), account.account_ref()),
                    "account_mode": account.account_mode,
                    "tax_universe": if account.is_paper() { "paper" } else { "real" },
                    "source": "config",
                }));
            }
        }
    }

    let conn = open_db()?;
    for table in ["auto_positions", "provider_fill_activities"] {
        if !table_exists(&conn, table)? {
            continue;
        }
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT provider, account_ref, account_mode, paper_account FROM {table}"
        ))?;
        let db_rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
            ))
        })?;
        for row in db_rows.filter_map(|row| row.ok()) {
            let key = format!("{}|{}|{}|{}", row.0, row.1, row.2, row.3);
            if seen.insert(key) {
                rows.push(serde_json::json!({
                    "provider": row.0,
                    "account_ref": row.1,
                    "selector": format!("{}:{}", row.0, row.1),
                    "account_mode": row.2,
                    "tax_universe": if row.3 { "paper" } else { "real" },
                    "source": table,
                }));
            }
        }
    }

    rows.sort_by(|a, b| {
        a["selector"]
            .as_str()
            .unwrap_or("")
            .cmp(b["selector"].as_str().unwrap_or(""))
    });
    Ok(rows)
}

pub fn cmd_tax_accounts(json: bool) -> anyhow::Result<()> {
    let accounts = known_tax_accounts()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "accounts": accounts }))?
        );
        return Ok(());
    }
    println!("Tax Accounts");
    println!("{:<24} {:<12} {:<8} Source", "Selector", "Mode", "Universe");
    println!("{}", "-".repeat(62));
    for account in &accounts {
        println!(
            "{:<24} {:<12} {:<8} {}",
            account["selector"].as_str().unwrap_or("?"),
            account["account_mode"].as_str().unwrap_or("?"),
            account["tax_universe"].as_str().unwrap_or("?"),
            account["source"].as_str().unwrap_or("?")
        );
    }
    println!();
    println!("Default tax estimate excludes paper accounts. Select a paper account explicitly with `--account` to simulate it.");
    Ok(())
}

fn bracket_rows(brackets: &[TaxBracket]) -> Vec<serde_json::Value> {
    brackets
        .iter()
        .scan(0.0, |lower, bracket| {
            let item = serde_json::json!({
                "lower": *lower,
                "upper": bracket.upper,
                "rate": bracket.rate,
            });
            if let Some(upper) = bracket.upper {
                *lower = upper;
            }
            Some(item)
        })
        .collect()
}

pub fn cmd_tax_show_brackets(year: i32, json: bool) -> anyhow::Result<()> {
    let app_config = config::load()?;
    let filing_status = app_config.tax.filing_status.as_deref().unwrap_or("single");
    let filing_status = FilingStatus::parse(filing_status)?;
    let estimated_income = app_config.tax.estimated_annual_income.unwrap_or(0.0);
    let table = tax_table(year, filing_status)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "year": year,
                "filing_status": filing_status.label(),
                "estimated_income": estimated_income,
                "bracket_config": table.source_path,
                "ordinary_income": bracket_rows(&table.ordinary),
                "short_term_capital_gains": "taxed as ordinary income using ordinary brackets",
                "long_term_capital_gains": bracket_rows(&table.capital_gains),
                "net_investment_income_tax": {
                    "rate": table.niit_rate,
                    "threshold": table.niit_threshold,
                    "rule": "3.8% on the lesser of net investment income or modified AGI above the threshold"
                },
                "current_income_rates": {
                    "short_term_marginal": marginal_rate(estimated_income, &table.ordinary),
                    "long_term_marginal": capital_gain_marginal_rate(estimated_income, &table.capital_gains),
                    "niit_possible": estimated_income > table.niit_threshold,
                }
            }))?
        );
        return Ok(());
    }

    println!("Federal Tax Brackets - {}", year);
    println!("Filing status: {}", filing_status.label());
    println!("Estimated annual income: {}", money(estimated_income));
    println!("Bracket config: {}", table.source_path.display());
    println!();
    println!("Short-term capital gains: taxed as ordinary income.");
    println!("Ordinary income brackets:");
    let mut lower = 0.0;
    for bracket in &table.ordinary {
        let upper = bracket
            .upper
            .map(money)
            .unwrap_or_else(|| "and above".to_string());
        println!("  {} to {}: {}", money(lower), upper, pct(bracket.rate));
        if let Some(upper) = bracket.upper {
            lower = upper;
        }
    }
    println!();
    println!("Long-term capital gains brackets:");
    let mut lower = 0.0;
    for bracket in &table.capital_gains {
        let upper = bracket
            .upper
            .map(money)
            .unwrap_or_else(|| "and above".to_string());
        println!("  {} to {}: {}", money(lower), upper, pct(bracket.rate));
        if let Some(upper) = bracket.upper {
            lower = upper;
        }
    }
    println!();
    println!(
        "Net Investment Income Tax: {} when income exceeds {}.",
        pct(table.niit_rate),
        money(table.niit_threshold)
    );
    println!(
        "Current configured income rates: short-term marginal {} | long-term marginal {} | NIIT {}",
        pct(marginal_rate(estimated_income, &table.ordinary)),
        pct(capital_gain_marginal_rate(
            estimated_income,
            &table.capital_gains
        )),
        if estimated_income > table.niit_threshold {
            "applies to taxable net investment income"
        } else {
            "does not apply before trading gains"
        }
    );
    Ok(())
}

fn calculate_estimate(
    table: &TaxYearTable,
    filing_status: FilingStatus,
    estimated_income: f64,
    include_paper: bool,
    excluded_paper_positions: usize,
    start: NaiveDate,
    end: NaiveDate,
    quarter_id: u8,
    period_label: &str,
    scope: &str,
    provider: &str,
    account_ref: &str,
    account_mode: &str,
    paper_account: i64,
    positions: &[&ClosedPosition],
) -> TaxEstimate {
    let mut short = TermTotals::default();
    let mut long = TermTotals::default();
    for position in positions {
        let held_days = (position.exit_date - position.entry_date).num_days();
        if held_days > 365 {
            add_to_totals(&mut long, position.pnl);
        } else {
            add_to_totals(&mut short, position.pnl);
        }
    }
    let netting = net_taxable(short.net, long.net);
    let ordinary_before = progressive_tax(estimated_income, &table.ordinary);
    let ordinary_after = progressive_tax(
        estimated_income + netting.taxable_short_term,
        &table.ordinary,
    );
    let short_term_tax = (ordinary_after - ordinary_before).max(0.0);
    let long_term_tax = capital_gain_tax(
        estimated_income + netting.taxable_short_term,
        netting.taxable_long_term,
        &table.capital_gains,
    );
    let niit_tax = niit_tax_for_trading_gains(
        table,
        estimated_income,
        netting.taxable_short_term,
        netting.taxable_long_term,
    );
    let total_tax = short_term_tax + long_term_tax + niit_tax;
    let ordinary_marginal_rate = marginal_rate(estimated_income, &table.ordinary);
    let effective_short_rate = if netting.taxable_short_term > 0.0 {
        short_term_tax / netting.taxable_short_term
    } else {
        0.0
    };
    let effective_long_rate = if netting.taxable_long_term > 0.0 {
        long_term_tax / netting.taxable_long_term
    } else {
        0.0
    };
    let niit_base = netting.taxable_short_term + netting.taxable_long_term;
    let niit_rate = if niit_base > 0.0 {
        niit_tax / niit_base
    } else {
        0.0
    };

    TaxEstimate {
        year: table.year,
        quarter: quarter_id,
        period_label: period_label.to_string(),
        period_start: start.format("%Y-%m-%d").to_string(),
        period_end: end.format("%Y-%m-%d").to_string(),
        scope: scope.to_string(),
        provider: provider.to_string(),
        account_ref: account_ref.to_string(),
        account_mode: account_mode.to_string(),
        paper_account,
        filing_status,
        filing_status_label: filing_status.label().to_string(),
        estimated_annual_income: estimated_income,
        include_paper_accounts_for_estimate: include_paper,
        excluded_paper_positions,
        total_net: short.net + long.net,
        taxable_after_netting: netting,
        rates: RateSummary {
            ordinary_marginal_rate,
            short_term_effective_rate: effective_short_rate,
            long_term_effective_rate: effective_long_rate,
            niit_rate,
            short_term_with_niit_effective_rate: effective_short_rate + niit_rate,
            long_term_with_niit_effective_rate: effective_long_rate + niit_rate,
        },
        estimated_federal_tax: FederalTaxEstimate {
            short_term: short_term_tax,
            long_term: long_term_tax,
            net_investment_income_tax: niit_tax,
            total: total_tax,
        },
        position_count: positions.len(),
        generated_at_utc: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        short_term: short,
        long_term: long,
    }
}

fn build_estimates_with_filters(
    year: i32,
    quarters: &[u8],
    account_filters: &[String],
) -> anyhow::Result<(
    TaxEstimate,
    Vec<TaxEstimate>,
    Vec<TaxEstimate>,
    usize,
    Vec<ClosedPosition>,
    TaxYearTable,
    FilingStatus,
    f64,
)> {
    let app_config = config::load()?;
    let account_tokens = normalize_account_filters(account_filters);
    let filing_status = app_config
        .tax
        .filing_status
        .as_deref()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "tax.filing_status is not set in {}.",
                config::config_path().display()
            )
        })
        .and_then(FilingStatus::parse)?;
    let estimated_income = app_config.tax.estimated_annual_income.ok_or_else(|| {
        anyhow::anyhow!(
            "tax.estimated_annual_income is not set in {}.",
            config::config_path().display()
        )
    })?;
    let include_paper = app_config
        .tax
        .include_paper_accounts_for_estimate
        .unwrap_or(false)
        || account_filters_may_include_paper(&account_tokens);
    let table = tax_table(year, filing_status)?;
    let (start, end, quarter_id, period_label) = tax_period_range(year, quarters)?;
    let conn = open_db()?;
    let (mut positions, excluded_paper_positions) =
        load_closed_positions(&conn, start, end, include_paper)?;
    positions.retain(|position| position_matches_account_filters(position, &account_tokens));
    let all_refs = positions.iter().collect::<Vec<_>>();

    let consolidated = calculate_estimate(
        &table,
        filing_status,
        estimated_income,
        include_paper,
        excluded_paper_positions,
        start,
        end,
        quarter_id,
        &period_label,
        "consolidated",
        "ALL",
        "ALL",
        "mixed",
        -1,
        &all_refs,
    );

    let mut by_provider: BTreeMap<String, Vec<&ClosedPosition>> = BTreeMap::new();
    let mut by_account: BTreeMap<(String, String, String, i64), Vec<&ClosedPosition>> =
        BTreeMap::new();
    for position in &positions {
        by_provider
            .entry(position.provider.clone())
            .or_default()
            .push(position);
        by_account
            .entry((
                position.provider.clone(),
                position.account_ref.clone(),
                position.account_mode.clone(),
                if position.paper_account { 1 } else { 0 },
            ))
            .or_default()
            .push(position);
    }
    let provider_estimates = by_provider
        .into_iter()
        .map(|(provider, positions)| {
            calculate_estimate(
                &table,
                filing_status,
                estimated_income,
                include_paper,
                0,
                start,
                end,
                quarter_id,
                &period_label,
                "provider",
                &provider,
                "ALL",
                "mixed",
                -1,
                &positions,
            )
        })
        .collect::<Vec<_>>();
    let account_estimates = by_account
        .into_iter()
        .map(
            |((provider, account_ref, account_mode, paper_account), positions)| {
                calculate_estimate(
                    &table,
                    filing_status,
                    estimated_income,
                    include_paper,
                    0,
                    start,
                    end,
                    quarter_id,
                    &period_label,
                    "account",
                    &provider,
                    &account_ref,
                    &account_mode,
                    paper_account,
                    &positions,
                )
            },
        )
        .collect::<Vec<_>>();
    let position_count = positions.len();
    Ok((
        consolidated,
        provider_estimates,
        account_estimates,
        position_count,
        positions,
        table,
        filing_status,
        estimated_income,
    ))
}

fn build_estimates(
    year: i32,
    quarters: &[u8],
) -> anyhow::Result<(TaxEstimate, Vec<TaxEstimate>, Vec<TaxEstimate>, usize)> {
    let (consolidated, providers, accounts, position_count, _, _, _, _) =
        build_estimates_with_filters(year, quarters, &[])?;
    Ok((consolidated, providers, accounts, position_count))
}

fn write_csv(estimates: &[TaxEstimate], year: i32, period_label: &str) -> anyhow::Result<PathBuf> {
    paths::ensure_runtime_dirs()?;
    let path = paths::data_dir().join(format!("tax_{}_{}.csv", year, period_label));
    let mut file = File::create(&path)?;
    writeln!(
        file,
        "year,quarter,period_label,period_start,period_end,scope,provider,account_ref,account_mode,paper_account,filing_status,estimated_annual_income,short_gains,short_losses,short_net,short_count,long_gains,long_losses,long_net,long_count,total_net,taxable_short_term,taxable_long_term,capital_loss_after_netting,ordinary_marginal_rate,short_term_effective_rate,long_term_effective_rate,niit_rate,short_term_with_niit_effective_rate,long_term_with_niit_effective_rate,estimated_short_term_tax,estimated_long_term_tax,estimated_niit_tax,estimated_total_tax,position_count,generated_at_utc"
    )?;
    for estimate in estimates {
        writeln!(
            file,
            "{},{},{},{},{},{},{},{},{},{},{:?},{:.2},{:.2},{:.2},{:.2},{},{:.2},{:.2},{:.2},{},{:.2},{:.2},{:.2},{:.2},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.2},{:.2},{:.2},{:.2},{},{}",
            estimate.year,
            estimate.quarter,
            estimate.period_label,
            estimate.period_start,
            estimate.period_end,
            estimate.scope,
            estimate.provider,
            estimate.account_ref,
            estimate.account_mode,
            estimate.paper_account,
            estimate.filing_status,
            estimate.estimated_annual_income,
            estimate.short_term.gains,
            estimate.short_term.losses,
            estimate.short_term.net,
            estimate.short_term.count,
            estimate.long_term.gains,
            estimate.long_term.losses,
            estimate.long_term.net,
            estimate.long_term.count,
            estimate.total_net,
            estimate.taxable_after_netting.taxable_short_term,
            estimate.taxable_after_netting.taxable_long_term,
            estimate.taxable_after_netting.capital_loss_after_netting,
            estimate.rates.ordinary_marginal_rate,
            estimate.rates.short_term_effective_rate,
            estimate.rates.long_term_effective_rate,
            estimate.rates.niit_rate,
            estimate.rates.short_term_with_niit_effective_rate,
            estimate.rates.long_term_with_niit_effective_rate,
            estimate.estimated_federal_tax.short_term,
            estimate.estimated_federal_tax.long_term,
            estimate.estimated_federal_tax.net_investment_income_tax,
            estimate.estimated_federal_tax.total,
            estimate.position_count,
            estimate.generated_at_utc
        )?;
    }
    Ok(path)
}

pub fn refresh_current_year_estimates() -> anyhow::Result<()> {
    let year = Utc::now().year();
    let today = Utc::now().date_naive();
    let mut estimates = Vec::new();
    let (consolidated, providers, accounts, _) = build_estimates(year, &[])?;
    estimates.push(consolidated);
    estimates.extend(providers);
    estimates.extend(accounts);
    for quarter in refreshable_quarters_for_date(year, today) {
        let quarters = [quarter];
        let (consolidated, providers, accounts, _) = build_estimates(year, &quarters)?;
        estimates.push(consolidated);
        estimates.extend(providers);
        estimates.extend(accounts);
    }
    save_estimates(&estimates)?;
    Ok(())
}

pub fn cmd_tax_show(
    _show: bool,
    year: i32,
    quarter: Option<String>,
    export: Option<String>,
    account_filters: Vec<String>,
    details: bool,
    json: bool,
) -> anyhow::Result<()> {
    let quarters = parse_quarters(quarter)?;
    let (
        consolidated,
        provider_estimates,
        account_estimates,
        position_count,
        positions,
        table,
        _filing_status,
        estimated_income,
    ) = build_estimates_with_filters(year, &quarters, &account_filters)?;
    let quarter_estimates = quarter_breakdown(year, &account_filters)?;
    let period_label = consolidated.period_label.clone();
    let mut all_estimates = Vec::new();
    all_estimates.push(consolidated.clone());
    all_estimates.extend(provider_estimates.clone());
    all_estimates.extend(account_estimates.clone());
    save_estimates(&all_estimates)?;

    let export_path = match export.as_deref() {
        Some("csv") => Some(write_csv(&all_estimates, year, &period_label)?),
        Some(other) => anyhow::bail!("unsupported tax export '{}'. Use --export csv.", other),
        None => None,
    };

    if json {
        let detail_rows = if details {
            positions
                .iter()
                .map(|position| {
                    serde_json::json!({
                        "provider": position.provider,
                        "account_ref": position.account_ref,
                        "account_mode": position.account_mode,
                        "tax_universe": if position.paper_account { "paper" } else { "real" },
                        "symbol": position.symbol,
                        "term": term_for_position(position),
                        "qty": position.qty,
                        "entry_date": position.entry_date.to_string(),
                        "entry_price": position.entry_price,
                        "exit_date": position.exit_date.to_string(),
                        "exit_price": position.exit_price,
                        "pnl": position.pnl,
                        "estimated_federal_tax_impact": operation_tax_impact(position, &table, estimated_income),
                        "source": position.source,
                    })
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "consolidated": consolidated,
                "by_provider": provider_estimates,
                "by_account": account_estimates,
                "by_quarter": quarter_estimates,
                "details": detail_rows,
                "account_filters": normalize_account_filters(&account_filters),
                "source_table": "auto_positions closed rows + provider_fill_activities FIFO fills",
                "tax_database": tax_db_path(),
                "export_path": export_path,
                "notes": [
                    "Short-term net gains are estimated as incremental ordinary income tax.",
                    "Long-term net gains use IRS 0%/15%/20% capital-gain brackets.",
                    "Net Investment Income Tax is estimated at 3.8% when income exceeds the filing-status threshold.",
                    "This is an estimate, not tax advice."
                ],
            }))?
        );
        return Ok(());
    }

    println!(
        "Federal Tax Estimate - {} {}",
        year,
        period_label.to_ascii_uppercase()
    );
    println!(
        "Period: {} to {}",
        consolidated.period_start, consolidated.period_end
    );
    println!("Filing status: {}", consolidated.filing_status_label);
    println!(
        "Estimated annual income: {}",
        money(consolidated.estimated_annual_income)
    );
    println!("Income assumption: taxable ordinary income before trading gains");
    println!("Data source: auto_positions closed rows + provider_fill_activities FIFO fills");
    let account_tokens = normalize_account_filters(&account_filters);
    println!(
        "Account filter: {}",
        if account_tokens.is_empty() {
            "all real accounts by default".to_string()
        } else {
            account_tokens.join(", ")
        }
    );
    println!(
        "Paper accounts: {}",
        if consolidated.include_paper_accounts_for_estimate {
            "included for simulation"
        } else {
            "excluded"
        }
    );
    if consolidated.excluded_paper_positions > 0 {
        println!(
            "Excluded paper positions: {}",
            consolidated.excluded_paper_positions
        );
    }
    println!();
    println!(
        "Short-term: gains {} | losses {} | net {} | positions {}",
        money(consolidated.short_term.gains),
        money(consolidated.short_term.losses),
        money(consolidated.short_term.net),
        consolidated.short_term.count
    );
    println!(
        "Long-term:  gains {} | losses {} | net {} | positions {}",
        money(consolidated.long_term.gains),
        money(consolidated.long_term.losses),
        money(consolidated.long_term.net),
        consolidated.long_term.count
    );
    println!("Total realized net: {}", money(consolidated.total_net));
    println!();
    println!(
        "Taxable after netting: short-term {} | long-term {} | unused net capital loss {}",
        money(consolidated.taxable_after_netting.taxable_short_term),
        money(consolidated.taxable_after_netting.taxable_long_term),
        money(
            consolidated
                .taxable_after_netting
                .capital_loss_after_netting
        )
    );
    println!(
        "Rates: short-term ordinary marginal {} | short effective {} | long effective {}",
        pct(consolidated.rates.ordinary_marginal_rate),
        pct(consolidated.rates.short_term_effective_rate),
        pct(consolidated.rates.long_term_effective_rate)
    );
    println!(
        "Additional federal tax: Net Investment Income Tax {} effective, threshold {}",
        pct(consolidated.rates.niit_rate),
        money(table.niit_threshold)
    );
    println!(
        "Rates with NIIT: short {} | long {}",
        pct(consolidated.rates.short_term_with_niit_effective_rate),
        pct(consolidated.rates.long_term_with_niit_effective_rate)
    );
    println!(
        "Estimated federal tax: short-term {} | long-term {} | NIIT {} | total {}",
        money(consolidated.estimated_federal_tax.short_term),
        money(consolidated.estimated_federal_tax.long_term),
        money(consolidated.estimated_federal_tax.net_investment_income_tax),
        money(consolidated.estimated_federal_tax.total)
    );
    if !quarter_estimates.is_empty() {
        println!();
        println!("Quarter breakdown:");
        println!(
            "{:<8} {:>12} {:>12} {:>12} {:>12} {:>8}",
            "Quarter", "Short net", "Long net", "Total net", "Fed tax", "Positions"
        );
        println!("{}", "-".repeat(76));
        for estimate in &quarter_estimates {
            println!(
                "{:<8} {:>12} {:>12} {:>12} {:>12} {:>8}",
                estimate.period_label.to_ascii_uppercase(),
                money(estimate.short_term.net),
                money(estimate.long_term.net),
                money(estimate.total_net),
                money(estimate.estimated_federal_tax.total),
                estimate.position_count
            );
        }
    }
    if details {
        println!();
        println!("Operation details:");
        println!(
            "{:<10} {:<8} {:<6} {:<8} {:<10} {:>10} {:<10} {:>10} {:>11} {:>11}",
            "Account",
            "Symbol",
            "Term",
            "Qty",
            "Entry",
            "Entry Px",
            "Exit",
            "Exit Px",
            "P&L",
            "Tax impact"
        );
        println!("{}", "-".repeat(116));
        for position in &positions {
            println!(
                "{:<10} {:<8} {:<6} {:<8.2} {:<10} {:>10} {:<10} {:>10} {:>11} {:>11}",
                position.account_ref,
                position.symbol,
                term_for_position(position),
                position.qty,
                position.entry_date,
                money(position.entry_price),
                position.exit_date,
                money(position.exit_price),
                money(position.pnl),
                money(operation_tax_impact(position, &table, estimated_income))
            );
        }
    }
    println!(
        "Saved estimates: {} consolidated/provider/account rows in {}",
        all_estimates.len(),
        tax_db_path().display()
    );
    if let Some(path) = export_path {
        println!("CSV export: {}", path.display());
    }
    println!("Positions included: {}", position_count);
    println!();
    println!("Note: estimate only. It does not replace Form 8949/Schedule D or CPA review.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshable_quarters_skip_future_current_year_quarters() {
        let today = NaiveDate::from_ymd_opt(2026, 5, 3).unwrap();
        assert_eq!(refreshable_quarters_for_date(2026, today), vec![1, 2]);
    }

    #[test]
    fn refreshable_quarters_include_all_past_year_quarters() {
        let today = NaiveDate::from_ymd_opt(2026, 5, 3).unwrap();
        assert_eq!(refreshable_quarters_for_date(2025, today), vec![1, 2, 3, 4]);
    }
}
