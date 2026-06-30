// Tax estimate and account-level realized gain/loss reporting.
//
// Function map:
// - tax_table()/validate_brackets(): load country bracket/rate data safely.
// - load_*positions(): build realized lots from provider/local history.
// - calculate_estimate(): applies short/long-term, netting, and NIIT rules.
// - cmd_tax_*(): CLI entrypoints for accounts, brackets, estimates, and CSV.

use crate::{compliance, config, origin, paths};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
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
    // Handles parse logic.
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

    // Handles label logic.
    fn label(self) -> &'static str {
        match self {
            Self::Single => "Single",
            Self::MarriedFilingJointly => "Married filing jointly / qualifying surviving spouse",
            Self::MarriedFilingSeparately => "Married filing separately",
            Self::HeadOfHousehold => "Head of household",
        }
    }

    // Builds or returns key configuration state.
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
    // Handles for status logic.
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
    // Handles for status logic.
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
    country: compliance::TaxCountry,
    profile: compliance::TaxCountryProfile,
    ordinary: Vec<TaxBracket>,
    capital_gains: Vec<TaxBracket>,
    niit_rate: f64,
    niit_threshold: f64,
    annual_exempt_amount: f64,
    tax_year_label: String,
    tax_period_basis: &'static str,
    taxable_gain_model: &'static str,
    lot_matching_rule: &'static str,
    rule_limitations: Vec<&'static str>,
    short_term_label: &'static str,
    long_term_label: &'static str,
    source_label: String,
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
    entry_execution_origin: origin::ExecutionOrigin,
    exit_execution_origin: origin::ExecutionOrigin,
    execution_origin: origin::ExecutionOrigin,
    asset_market_country: compliance::TaxCountry,
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
    execution_origin: origin::ExecutionOrigin,
    asset_market_country: compliance::TaxCountry,
}

#[derive(Debug, Clone)]
struct MatchBuyLot {
    date: NaiveDate,
    qty_remaining: f64,
    price: f64,
    execution_origin: origin::ExecutionOrigin,
    pooled: bool,
}

#[derive(Debug, Clone)]
struct MatchSellLot {
    provider: String,
    account_ref: String,
    account_mode: String,
    paper_account: bool,
    symbol: String,
    date: NaiveDate,
    qty_remaining: f64,
    price: f64,
    execution_origin: origin::ExecutionOrigin,
    asset_market_country: compliance::TaxCountry,
}

#[derive(Debug, Default, Clone)]
struct Section104Pool {
    qty: f64,
    cost: f64,
}

#[derive(Debug, Clone)]
struct OpenLot {
    date: NaiveDate,
    qty: f64,
    price: f64,
    execution_origin: origin::ExecutionOrigin,
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
    tax_country_code: String,
    tax_country_name: String,
    currency_code: String,
    currency_symbol: String,
    tax_rule_name: String,
    tax_rule_summary: String,
    tax_year_label: String,
    tax_period_basis: String,
    taxable_gain_model: String,
    lot_matching_rule: String,
    rule_limitations: Vec<String>,
    short_term_label: String,
    long_term_label: String,
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

const UK_BASIC_RATE_BAND_UPPER: f64 = 37_700.0;
const SG_RESIDENT_INCOME_BRACKETS: [TaxBracket; 13] = [
    TaxBracket {
        upper: Some(20_000.0),
        rate: 0.0,
    },
    TaxBracket {
        upper: Some(30_000.0),
        rate: 0.02,
    },
    TaxBracket {
        upper: Some(40_000.0),
        rate: 0.035,
    },
    TaxBracket {
        upper: Some(80_000.0),
        rate: 0.07,
    },
    TaxBracket {
        upper: Some(120_000.0),
        rate: 0.115,
    },
    TaxBracket {
        upper: Some(160_000.0),
        rate: 0.15,
    },
    TaxBracket {
        upper: Some(200_000.0),
        rate: 0.18,
    },
    TaxBracket {
        upper: Some(240_000.0),
        rate: 0.19,
    },
    TaxBracket {
        upper: Some(280_000.0),
        rate: 0.195,
    },
    TaxBracket {
        upper: Some(320_000.0),
        rate: 0.20,
    },
    TaxBracket {
        upper: Some(500_000.0),
        rate: 0.22,
    },
    TaxBracket {
        upper: Some(1_000_000.0),
        rate: 0.23,
    },
    TaxBracket {
        upper: None,
        rate: 0.24,
    },
];

// Returns country-specific tax year label.
fn tax_year_label(country: compliance::TaxCountry, year: i32) -> String {
    match country {
        compliance::TaxCountry::Gb => format!("UK tax year {}-{}", year, year + 1),
        compliance::TaxCountry::Sg => format!("Singapore YA {}", year),
        _ => year.to_string(),
    }
}

// Returns country-specific tax period basis.
fn tax_period_basis(country: compliance::TaxCountry) -> &'static str {
    compliance::tax_country_profile(country).tax_period_basis
}

// Returns taxable gain model summary.
fn taxable_gain_model(country: compliance::TaxCountry) -> &'static str {
    compliance::tax_country_profile(country).taxable_gain_model
}

// Returns lot matching rule summary.
fn lot_matching_rule(country: compliance::TaxCountry) -> &'static str {
    compliance::tax_country_profile(country).lot_matching_rule
}

// Returns country rule limitations that require taxpayer facts or external data.
fn rule_limitations(country: compliance::TaxCountry) -> Vec<&'static str> {
    compliance::tax_country_profile(country)
        .rule_limitations
        .to_vec()
}

// Returns user-facing bucket labels for the two existing estimate buckets.
fn bucket_labels(country: compliance::TaxCountry) -> (&'static str, &'static str) {
    match country {
        compliance::TaxCountry::Us => ("Short-term", "Long-term"),
        compliance::TaxCountry::Br => ("Day trade", "Normal/swing"),
        compliance::TaxCountry::Sg => ("Revenue/trading", "Capital/non-taxable"),
        compliance::TaxCountry::Gb => ("Same-year disposals", "Pooled/longer-held disposals"),
    }
}

// Parses provider exchange names into a market country when possible.
fn market_country_from_exchange(exchange: &str) -> Option<compliance::TaxCountry> {
    match exchange.trim().to_ascii_uppercase().as_str() {
        "B3"
        | "BOVESPA"
        | "BMFBOVESPA"
        | "SAO"
        | "SAO PAULO"
        | "SAO PAULO STOCK EXCHANGE"
        | "XBSP" => Some(compliance::TaxCountry::Br),
        "LSE" | "XLON" | "LONDON" | "LONDON STOCK EXCHANGE" => Some(compliance::TaxCountry::Gb),
        "SGX" | "XSES" | "SINGAPORE" | "SINGAPORE EXCHANGE" => Some(compliance::TaxCountry::Sg),
        "NYSE" | "NASDAQ" | "AMEX" | "ARCA" | "BATS" | "IEX" | "OTC" | "OTCM" | "PINK" => {
            Some(compliance::TaxCountry::Us)
        }
        _ => None,
    }
}

// Parses common market suffixes into a market country when possible.
fn market_country_from_symbol(symbol: &str) -> Option<compliance::TaxCountry> {
    let upper = symbol.trim().to_ascii_uppercase();
    if upper.ends_with(".SA") || upper.ends_with(".BVMF") {
        return Some(compliance::TaxCountry::Br);
    }
    if upper.ends_with(".L") || upper.ends_with(".LN") {
        return Some(compliance::TaxCountry::Gb);
    }
    if upper.ends_with(".SI") {
        return Some(compliance::TaxCountry::Sg);
    }
    None
}

// Parses provider names into a market country when possible.
fn market_country_from_provider(provider: &str) -> Option<compliance::TaxCountry> {
    let normalized = provider.trim().to_ascii_lowercase();
    if normalized.contains("b3")
        || normalized.contains("bovespa")
        || normalized.contains("bvmf")
        || normalized.contains("brasil")
    {
        return Some(compliance::TaxCountry::Br);
    }
    if normalized.contains("sgx") || normalized.contains("singapore") {
        return Some(compliance::TaxCountry::Sg);
    }
    if normalized.contains("lse") || normalized.contains("london") || normalized.contains("uk") {
        return Some(compliance::TaxCountry::Gb);
    }
    if normalized.contains("alpaca") {
        return Some(compliance::TaxCountry::Us);
    }
    None
}

// Infers the market country for a realized position from local metadata.
fn infer_asset_market_country(
    conn: &Connection,
    residency_country: compliance::TaxCountry,
    provider: &str,
    symbol: &str,
) -> compliance::TaxCountry {
    if let Some(country) = market_country_from_symbol(symbol) {
        return country;
    }
    if table_exists(conn, "assets").unwrap_or(false) {
        let exchange = conn
            .query_row(
                "SELECT exchange FROM assets WHERE UPPER(symbol)=UPPER(?1)",
                params![symbol],
                |row| row.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        if let Some(exchange) = exchange {
            if let Some(country) = market_country_from_exchange(&exchange) {
                return country;
            }
        }
    }
    market_country_from_provider(provider).unwrap_or(residency_country)
}

// Returns UK annual exempt amount for individual share CGT by tax-year start.
fn uk_annual_exempt_amount(year: i32) -> f64 {
    match year {
        i32::MIN..=2022 => 12_300.0,
        2023 => 6_000.0,
        _ => 3_000.0,
    }
}

// Returns UK share CGT rates by tax-year start.
fn uk_share_cgt_rates(year: i32) -> (f64, f64) {
    if year >= 2024 {
        (0.18, 0.24)
    } else {
        (0.10, 0.20)
    }
}

// Handles tax table data or configuration.
fn tax_table(
    year: i32,
    status: FilingStatus,
    country: compliance::TaxCountry,
) -> anyhow::Result<TaxYearTable> {
    let profile = compliance::tax_country_profile(country);
    if country != compliance::TaxCountry::Us {
        let (ordinary, capital_gains, annual_exempt_amount, source_label) = match country {
            compliance::TaxCountry::Br => (
                vec![TaxBracket {
                    upper: None,
                    rate: 0.20,
                }],
                vec![TaxBracket {
                    upper: None,
                    rate: 0.15,
                }],
                0.0,
                "built-in Brazil Lei 14.754/2023 foreign financial investment 15% model"
                    .to_string(),
            ),
            compliance::TaxCountry::Sg => (
                SG_RESIDENT_INCOME_BRACKETS.to_vec(),
                SG_RESIDENT_INCOME_BRACKETS.to_vec(),
                0.0,
                "built-in Singapore resident individual income tax rates".to_string(),
            ),
            compliance::TaxCountry::Gb => (
                vec![TaxBracket {
                    upper: None,
                    rate: 0.0,
                }],
                vec![
                    TaxBracket {
                        upper: Some(UK_BASIC_RATE_BAND_UPPER),
                        rate: uk_share_cgt_rates(year).0,
                    },
                    TaxBracket {
                        upper: None,
                        rate: uk_share_cgt_rates(year).1,
                    },
                ],
                uk_annual_exempt_amount(year),
                format!(
                    "built-in UK share CGT {:.0}%/{:.0}% model with annual exempt amount",
                    uk_share_cgt_rates(year).0 * 100.0,
                    uk_share_cgt_rates(year).1 * 100.0
                ),
            ),
            compliance::TaxCountry::Us => unreachable!(),
        };
        validate_brackets(year, status, "ordinary_income", &ordinary)?;
        validate_brackets(year, status, "capital_gains", &capital_gains)?;
        let (short_term_label, long_term_label) = bucket_labels(country);
        return Ok(TaxYearTable {
            year,
            country,
            profile,
            ordinary,
            capital_gains,
            niit_rate: 0.0,
            niit_threshold: f64::INFINITY,
            annual_exempt_amount,
            tax_year_label: tax_year_label(country, year),
            tax_period_basis: tax_period_basis(country),
            taxable_gain_model: taxable_gain_model(country),
            lot_matching_rule: lot_matching_rule(country),
            rule_limitations: rule_limitations(country),
            short_term_label,
            long_term_label,
            source_label,
        });
    }

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
    let (short_term_label, long_term_label) = bucket_labels(country);
    Ok(TaxYearTable {
        year,
        country,
        profile,
        ordinary,
        capital_gains,
        niit_rate: year_config.net_investment_income_tax.rate,
        niit_threshold: year_config
            .net_investment_income_tax
            .thresholds
            .for_status(status),
        annual_exempt_amount: 0.0,
        tax_year_label: tax_year_label(country, year),
        tax_period_basis: tax_period_basis(country),
        taxable_gain_model: taxable_gain_model(country),
        lot_matching_rule: lot_matching_rule(country),
        rule_limitations: rule_limitations(country),
        short_term_label,
        long_term_label,
        source_label: path.display().to_string(),
    })
}

// Validates brackets against supported rules.
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

// Builds quarter range tax periods.
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

// Parses quarters from user or provider input.
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

// Normalizes account filters into canonical form.
fn normalize_account_filters(filters: &[String]) -> Vec<String> {
    filters
        .iter()
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

// Handles account filters may include paper matching or metadata.
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

// Handles position matches account filters logic.
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

// Handles country-specific tax period range data or configuration.
fn tax_period_range_for(
    country: compliance::TaxCountry,
    year: i32,
    quarters: &[u8],
) -> anyhow::Result<(NaiveDate, NaiveDate, u8, String)> {
    match country {
        compliance::TaxCountry::Gb => tax_period_range_gb(year, quarters),
        compliance::TaxCountry::Sg => tax_period_range_calendar(year - 1, quarters, Some(year)),
        compliance::TaxCountry::Us | compliance::TaxCountry::Br => {
            tax_period_range_calendar(year, quarters, None)
        }
    }
}

// Handles calendar-year or basis-year tax periods.
fn tax_period_range_calendar(
    calendar_year: i32,
    quarters: &[u8],
    assessment_year: Option<i32>,
) -> anyhow::Result<(NaiveDate, NaiveDate, u8, String)> {
    let current_year = Utc::now().year();
    let today = Utc::now().date_naive();
    let (start, mut end, quarter_id, label) = if quarters.is_empty() {
        (
            NaiveDate::from_ymd_opt(calendar_year, 1, 1)
                .ok_or_else(|| anyhow::anyhow!("invalid year {}", calendar_year))?,
            NaiveDate::from_ymd_opt(calendar_year, 12, 31)
                .ok_or_else(|| anyhow::anyhow!("invalid year {}", calendar_year))?,
            0,
            "year_to_date".to_string(),
        )
    } else {
        let first = *quarters.first().unwrap();
        let last = *quarters.last().unwrap();
        let (start, _) = quarter_range(calendar_year, first)?;
        let (_, end) = quarter_range(calendar_year, last)?;
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
    if calendar_year == current_year {
        end = end.min(today);
    }
    if end < start {
        anyhow::bail!(
            "selected tax period {} starts in the future for current year {}",
            label,
            assessment_year.unwrap_or(calendar_year)
        );
    }
    Ok((start, end, quarter_id, label))
}

// Handles UK tax-year periods (6 April through 5 April).
fn tax_period_range_gb(
    year: i32,
    quarters: &[u8],
) -> anyhow::Result<(NaiveDate, NaiveDate, u8, String)> {
    let today = Utc::now().date_naive();
    let quarter_dates = [
        (
            NaiveDate::from_ymd_opt(year, 4, 6).unwrap(),
            NaiveDate::from_ymd_opt(year, 7, 5).unwrap(),
        ),
        (
            NaiveDate::from_ymd_opt(year, 7, 6).unwrap(),
            NaiveDate::from_ymd_opt(year, 10, 5).unwrap(),
        ),
        (
            NaiveDate::from_ymd_opt(year, 10, 6).unwrap(),
            NaiveDate::from_ymd_opt(year + 1, 1, 5).unwrap(),
        ),
        (
            NaiveDate::from_ymd_opt(year + 1, 1, 6).unwrap(),
            NaiveDate::from_ymd_opt(year + 1, 4, 5).unwrap(),
        ),
    ];
    let (start, mut end, quarter_id, label) = if quarters.is_empty() {
        (
            quarter_dates[0].0,
            quarter_dates[3].1,
            0,
            "tax_year_to_date".to_string(),
        )
    } else {
        let first = *quarters.first().unwrap();
        let last = *quarters.last().unwrap();
        if !(1..=4).contains(&first) || !(1..=4).contains(&last) {
            anyhow::bail!("--quarter must contain only values 1, 2, 3, or 4");
        }
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
        (
            quarter_dates[(first - 1) as usize].0,
            quarter_dates[(last - 1) as usize].1,
            quarter_id,
            label,
        )
    };
    if today >= start && today <= end {
        end = today;
    }
    if end < start || today < start {
        anyhow::bail!(
            "selected UK tax period {} starts in the future for tax year {}-{}",
            label,
            year,
            year + 1
        );
    }
    Ok((start, end, quarter_id, label))
}

// Opens db with the configured runtime settings.
fn open_db() -> anyhow::Result<Connection> {
    let _ = paths::ensure_state_dir()?;
    let db_path = paths::scanner_db_path();
    let conn = Connection::open(&db_path)?;
    conn.execute_batch(&config::sqlite_runtime_pragma_sql())?;
    let _ = paths::harden_sqlite_files(&db_path);
    Ok(conn)
}

// Handles tax db path data or configuration.
fn tax_db_path() -> PathBuf {
    paths::db_dir().join("tax.db")
}

// Opens tax db with the configured runtime settings.
fn open_tax_db() -> anyhow::Result<Connection> {
    paths::ensure_runtime_dirs()?;
    let db_path = tax_db_path();
    let conn = Connection::open(&db_path)?;
    conn.execute_batch(&format!(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; {}
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
            tax_country_code TEXT NOT NULL DEFAULT 'US',
            tax_country_name TEXT NOT NULL DEFAULT 'United States',
            currency_code TEXT NOT NULL DEFAULT 'USD',
            currency_symbol TEXT NOT NULL DEFAULT '$',
            tax_rule_name TEXT NOT NULL DEFAULT 'US federal capital gains estimate',
            tax_rule_summary TEXT NOT NULL DEFAULT '',
            tax_year_label TEXT NOT NULL DEFAULT '',
            tax_period_basis TEXT NOT NULL DEFAULT 'calendar_year',
            taxable_gain_model TEXT NOT NULL DEFAULT '',
            lot_matching_rule TEXT NOT NULL DEFAULT '',
            short_term_label TEXT NOT NULL DEFAULT 'Short-term',
            long_term_label TEXT NOT NULL DEFAULT 'Long-term',
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
        config::sqlite_runtime_pragma_sql()
    ))?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "tax_country_code",
        "tax_country_code TEXT NOT NULL DEFAULT 'US'",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "tax_country_name",
        "tax_country_name TEXT NOT NULL DEFAULT 'United States'",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "currency_code",
        "currency_code TEXT NOT NULL DEFAULT 'USD'",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "currency_symbol",
        "currency_symbol TEXT NOT NULL DEFAULT '$'",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "tax_rule_name",
        "tax_rule_name TEXT NOT NULL DEFAULT 'US federal capital gains estimate'",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "tax_rule_summary",
        "tax_rule_summary TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "tax_year_label",
        "tax_year_label TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "tax_period_basis",
        "tax_period_basis TEXT NOT NULL DEFAULT 'calendar_year'",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "taxable_gain_model",
        "taxable_gain_model TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "lot_matching_rule",
        "lot_matching_rule TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "short_term_label",
        "short_term_label TEXT NOT NULL DEFAULT 'Short-term'",
    )?;
    ensure_tax_column(
        &conn,
        "tax_estimates",
        "long_term_label",
        "long_term_label TEXT NOT NULL DEFAULT 'Long-term'",
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
    let _ = paths::harden_sqlite_files(&db_path);
    Ok(conn)
}

// Handles tax table columns data or configuration.
fn tax_table_columns(conn: &Connection, table: &str) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|row| row.ok())
        .collect();
    Ok(columns)
}

// Ensures tax column exists or meets required invariants.
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

// Saves estimates to persistent storage.
fn save_estimates(estimates: &[TaxEstimate]) -> anyhow::Result<()> {
    let conn = open_tax_db()?;
    for estimate in estimates {
        conn.execute(
            "INSERT OR REPLACE INTO tax_estimates (
                year, quarter, period_label, period_start, period_end, scope, provider, account_ref,
                account_mode, paper_account, tax_country_code, tax_country_name, currency_code,
                currency_symbol, tax_rule_name, tax_rule_summary, tax_year_label, tax_period_basis,
                taxable_gain_model, lot_matching_rule, short_term_label, long_term_label,
                filing_status, filing_status_label, estimated_annual_income,
                include_paper_accounts_for_estimate,
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
                ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43, ?44,
                ?45, ?46, ?47, ?48, ?49, ?50, ?51
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
                estimate.tax_country_code,
                estimate.tax_country_name,
                estimate.currency_code,
                estimate.currency_symbol,
                estimate.tax_rule_name,
                estimate.tax_rule_summary,
                estimate.tax_year_label,
                estimate.tax_period_basis,
                estimate.taxable_gain_model,
                estimate.lot_matching_rule,
                estimate.short_term_label,
                estimate.long_term_label,
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

// Handles table exists database metadata.
fn table_exists(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        params![table],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

// Loads closed positions using the country-specific matching engine.
fn load_closed_positions_for_country(
    conn: &Connection,
    start: NaiveDate,
    end: NaiveDate,
    include_paper: bool,
    country: compliance::TaxCountry,
) -> anyhow::Result<(Vec<ClosedPosition>, usize)> {
    if country == compliance::TaxCountry::Gb && table_exists(conn, "provider_fill_activities")? {
        let (positions, excluded_paper) =
            load_gb_share_matching_positions(conn, start, end, include_paper)?;
        if !positions.is_empty() {
            return Ok((positions, excluded_paper));
        }
    }
    load_closed_positions(conn, start, end, include_paper, country)
}

// Loads closed positions from storage or configuration.
fn load_closed_positions(
    conn: &Connection,
    start: NaiveDate,
    end: NaiveDate,
    include_paper: bool,
    residency_country: compliance::TaxCountry,
) -> anyhow::Result<(Vec<ClosedPosition>, usize)> {
    let mut positions = Vec::new();
    let mut excluded_paper = 0usize;
    crate::auto::init_auto_tables(conn)?;

    if !table_exists(conn, "auto_positions")? {
        return load_provider_fill_positions(conn, start, end, include_paper, residency_country);
    }

    let start_str = start.format("%Y-%m-%d").to_string();
    let end_str = end.format("%Y-%m-%d").to_string();
    let mut stmt = conn.prepare(
        "SELECT provider, account_ref, account_mode, paper_account,
                entry_date, exit_date, COALESCE(pnl, 0.0), symbol,
                CAST(COALESCE(shares, 0) AS REAL), COALESCE(entry_price, 0.0), COALESCE(exit_price, 0.0),
                COALESCE(entry_execution_origin, 'mlai_auto'),
                COALESCE(exit_execution_origin, execution_origin, 'mlai_auto'),
                COALESCE(execution_origin, 'mlai_auto')
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
        let provider = row.get::<_, String>(0)?;
        let symbol = row.get::<_, String>(7)?.to_ascii_uppercase();
        let asset_market_country =
            infer_asset_market_country(conn, residency_country, &provider, &symbol);
        positions.push(ClosedPosition {
            provider,
            account_ref: row.get(1)?,
            account_mode: row.get(2)?,
            paper_account,
            symbol,
            qty: row.get::<_, f64>(8)?,
            entry_date: NaiveDate::parse_from_str(&row.get::<_, String>(4)?, "%Y-%m-%d")?,
            entry_price: row.get(9)?,
            exit_date: NaiveDate::parse_from_str(&exit_date, "%Y-%m-%d")?,
            exit_price: row.get(10)?,
            pnl: row.get(6)?,
            entry_execution_origin: origin::ExecutionOrigin::parse(
                &row.get::<_, String>(11)
                    .unwrap_or_else(|_| "mlai_auto".to_string()),
            ),
            exit_execution_origin: origin::ExecutionOrigin::parse(
                &row.get::<_, String>(12)
                    .unwrap_or_else(|_| "mlai_auto".to_string()),
            ),
            execution_origin: origin::ExecutionOrigin::parse(
                &row.get::<_, String>(13)
                    .unwrap_or_else(|_| "mlai_auto".to_string()),
            ),
            asset_market_country,
            source: "auto_positions".to_string(),
        });
    }

    let (provider_positions, provider_excluded_paper) =
        load_provider_fill_positions(conn, start, end, include_paper, residency_country)?;
    positions.extend(provider_positions);
    excluded_paper += provider_excluded_paper;

    Ok((positions, excluded_paper))
}

// Sorts realized tax operations newest first for CLI, API JSON, and dashboard output.
fn sort_closed_positions_newest_first(positions: &mut [ClosedPosition]) {
    positions.sort_by(|a, b| {
        b.exit_date
            .cmp(&a.exit_date)
            .then_with(|| b.entry_date.cmp(&a.entry_date))
            .then_with(|| a.provider.cmp(&b.provider))
            .then_with(|| a.account_ref.cmp(&b.account_ref))
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
}

// Loads provider fill positions from storage or configuration.
fn load_provider_fill_positions(
    conn: &Connection,
    start: NaiveDate,
    end: NaiveDate,
    include_paper: bool,
    residency_country: compliance::TaxCountry,
) -> anyhow::Result<(Vec<ClosedPosition>, usize)> {
    if !table_exists(conn, "provider_fill_activities")? {
        return Ok((Vec::new(), 0));
    }
    let end_str = end.format("%Y-%m-%d").to_string();
    let mut stmt = conn.prepare(
        "SELECT provider, account_ref, account_mode, paper_account, symbol, side,
                COALESCE(qty, 0.0), COALESCE(price, 0.0), transaction_time,
                COALESCE(execution_origin, 'provider_external')
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
            let provider = row.get::<_, String>(0)?;
            let symbol = row.get::<_, String>(4)?.to_ascii_uppercase();
            let asset_market_country =
                infer_asset_market_country(conn, residency_country, &provider, &symbol);
            Ok(FillActivity {
                provider,
                account_ref: row.get(1)?,
                account_mode: row.get(2)?,
                paper_account: row.get::<_, i64>(3).unwrap_or(1) != 0,
                symbol,
                side: row.get::<_, String>(5)?.to_ascii_lowercase(),
                qty: row.get(6)?,
                price: row.get(7)?,
                date,
                execution_origin: origin::ExecutionOrigin::parse(
                    &row.get::<_, String>(9)
                        .unwrap_or_else(|_| "provider_external".to_string()),
                ),
                asset_market_country,
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
                execution_origin: fill.execution_origin,
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
                entry_execution_origin: lot.execution_origin,
                exit_execution_origin: fill.execution_origin,
                execution_origin: if lot.execution_origin == fill.execution_origin {
                    fill.execution_origin
                } else {
                    origin::ExecutionOrigin::Mixed
                },
                asset_market_country: fill.asset_market_country,
                source: "provider_fill_activities".to_string(),
            });
        }
    }

    Ok((positions, excluded_paper))
}

// Builds UK CGT disposal rows from synced fills using HMRC share identification.
fn load_gb_share_matching_positions(
    conn: &Connection,
    start: NaiveDate,
    end: NaiveDate,
    include_paper: bool,
) -> anyhow::Result<(Vec<ClosedPosition>, usize)> {
    let end_plus_30 = (end + Duration::days(30)).format("%Y-%m-%d").to_string();
    let mut stmt = conn.prepare(
        "SELECT provider, account_ref, account_mode, paper_account, symbol, side,
                COALESCE(qty, 0.0), COALESCE(price, 0.0), transaction_time,
                COALESCE(execution_origin, 'provider_external')
         FROM provider_fill_activities
         WHERE transaction_time IS NOT NULL
           AND substr(transaction_time, 1, 10) <= ?1
         ORDER BY transaction_time, activity_id",
    )?;
    let fills = stmt
        .query_map(params![end_plus_30], |row| {
            let ts: String = row.get(8)?;
            let date_part = ts.get(0..10).unwrap_or("");
            let date = NaiveDate::parse_from_str(date_part, "%Y-%m-%d").map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?;
            let provider = row.get::<_, String>(0)?;
            let symbol = row.get::<_, String>(4)?.to_ascii_uppercase();
            let asset_market_country =
                infer_asset_market_country(conn, compliance::TaxCountry::Gb, &provider, &symbol);
            Ok(FillActivity {
                provider,
                account_ref: row.get(1)?,
                account_mode: row.get(2)?,
                paper_account: row.get::<_, i64>(3).unwrap_or(1) != 0,
                symbol,
                side: row.get::<_, String>(5)?.to_ascii_lowercase(),
                qty: row.get(6)?,
                price: row.get(7)?,
                date,
                execution_origin: origin::ExecutionOrigin::parse(
                    &row.get::<_, String>(9)
                        .unwrap_or_else(|_| "provider_external".to_string()),
                ),
                asset_market_country,
            })
        })?
        .filter_map(|row| row.ok())
        .filter(|fill| fill.qty > 0.0 && fill.price > 0.0)
        .collect::<Vec<_>>();

    let mut buys: BTreeMap<(String, String), Vec<MatchBuyLot>> = BTreeMap::new();
    let mut sells: BTreeMap<(String, String), Vec<MatchSellLot>> = BTreeMap::new();
    let mut excluded_paper = 0usize;

    for fill in fills {
        if fill.paper_account && !include_paper {
            if fill.side == "sell" && fill.date >= start && fill.date <= end {
                excluded_paper += 1;
            }
            continue;
        }
        let universe = if fill.paper_account {
            format!("paper:{}:{}", fill.provider, fill.account_ref)
        } else {
            "real".to_string()
        };
        let key = (universe, fill.symbol.clone());
        match fill.side.as_str() {
            "buy" => buys.entry(key).or_default().push(MatchBuyLot {
                date: fill.date,
                qty_remaining: fill.qty,
                price: fill.price,
                execution_origin: fill.execution_origin,
                pooled: false,
            }),
            "sell" => sells.entry(key).or_default().push(MatchSellLot {
                provider: fill.provider,
                account_ref: fill.account_ref,
                account_mode: fill.account_mode,
                paper_account: fill.paper_account,
                symbol: fill.symbol,
                date: fill.date,
                qty_remaining: fill.qty,
                price: fill.price,
                execution_origin: fill.execution_origin,
                asset_market_country: fill.asset_market_country,
            }),
            _ => {}
        }
    }

    let mut positions = Vec::new();
    let keys = sells.keys().cloned().collect::<Vec<_>>();
    for key in keys {
        let Some(sell_lots) = sells.get_mut(&key) else {
            continue;
        };
        sell_lots.sort_by(|a, b| a.date.cmp(&b.date));
        let buy_lots = buys.entry(key).or_default();
        buy_lots.sort_by(|a, b| a.date.cmp(&b.date));

        // HMRC same-day identification has priority over later 30-day matching.
        for sell in sell_lots.iter_mut() {
            match_buy_lots_to_sell(
                buy_lots,
                sell,
                start,
                end,
                &mut positions,
                "hmrc_same_day",
                |buy, sell_date| buy.date == sell_date,
            );
        }

        let mut pool = Section104Pool::default();
        let dates = sell_lots
            .iter()
            .filter(|sell| sell.qty_remaining > 0.0000001)
            .map(|sell| sell.date)
            .collect::<BTreeSet<_>>();
        for sell_date in dates {
            for sell in sell_lots
                .iter_mut()
                .filter(|sell| sell.date == sell_date && sell.qty_remaining > 0.0000001)
            {
                match_buy_lots_to_sell(
                    buy_lots,
                    sell,
                    start,
                    end,
                    &mut positions,
                    "hmrc_30_day",
                    |buy, date| buy.date > date && buy.date <= date + Duration::days(30),
                );
            }

            for buy in buy_lots
                .iter_mut()
                .filter(|buy| !buy.pooled && buy.date < sell_date && buy.qty_remaining > 0.0000001)
            {
                pool.qty += buy.qty_remaining;
                pool.cost += buy.qty_remaining * buy.price;
                buy.qty_remaining = 0.0;
                buy.pooled = true;
            }

            for sell in sell_lots
                .iter_mut()
                .filter(|sell| sell.date == sell_date && sell.qty_remaining > 0.0000001)
            {
                match_section104_pool_to_sell(&mut pool, sell, start, end, &mut positions);
            }
        }
    }

    Ok((positions, excluded_paper))
}

// Matches buy lots to one UK disposal using a supplied HMRC identification rule.
fn match_buy_lots_to_sell(
    buy_lots: &mut [MatchBuyLot],
    sell: &mut MatchSellLot,
    start: NaiveDate,
    end: NaiveDate,
    positions: &mut Vec<ClosedPosition>,
    source: &'static str,
    predicate: impl Fn(&MatchBuyLot, NaiveDate) -> bool,
) {
    for buy in buy_lots
        .iter_mut()
        .filter(|buy| buy.qty_remaining > 0.0000001 && predicate(buy, sell.date))
    {
        if sell.qty_remaining <= 0.0000001 {
            break;
        }
        let qty = sell.qty_remaining.min(buy.qty_remaining);
        sell.qty_remaining -= qty;
        buy.qty_remaining -= qty;
        push_matched_position(
            positions,
            sell,
            buy.date,
            buy.price,
            qty,
            buy.execution_origin,
            source,
            start,
            end,
        );
    }
}

// Matches one UK disposal to the Section 104 pool.
fn match_section104_pool_to_sell(
    pool: &mut Section104Pool,
    sell: &mut MatchSellLot,
    start: NaiveDate,
    end: NaiveDate,
    positions: &mut Vec<ClosedPosition>,
) {
    if pool.qty <= 0.0000001 || sell.qty_remaining <= 0.0000001 {
        return;
    }
    let avg_price = pool.cost / pool.qty;
    let qty = sell.qty_remaining.min(pool.qty);
    sell.qty_remaining -= qty;
    pool.qty -= qty;
    pool.cost = (pool.cost - avg_price * qty).max(0.0);
    push_matched_position(
        positions,
        sell,
        sell.date,
        avg_price,
        qty,
        origin::ExecutionOrigin::Mixed,
        "hmrc_section_104",
        start,
        end,
    );
}

// Adds a country-matched realized position when the disposal falls in-period.
fn push_matched_position(
    positions: &mut Vec<ClosedPosition>,
    sell: &MatchSellLot,
    entry_date: NaiveDate,
    entry_price: f64,
    qty: f64,
    entry_execution_origin: origin::ExecutionOrigin,
    source: &'static str,
    start: NaiveDate,
    end: NaiveDate,
) {
    if sell.date < start || sell.date > end || qty <= 0.0000001 {
        return;
    }
    positions.push(ClosedPosition {
        provider: sell.provider.clone(),
        account_ref: sell.account_ref.clone(),
        account_mode: sell.account_mode.clone(),
        paper_account: sell.paper_account,
        symbol: sell.symbol.clone(),
        qty,
        entry_date,
        entry_price,
        exit_date: sell.date,
        exit_price: sell.price,
        pnl: (sell.price - entry_price) * qty,
        entry_execution_origin,
        exit_execution_origin: sell.execution_origin,
        execution_origin: if entry_execution_origin == sell.execution_origin {
            sell.execution_origin
        } else {
            origin::ExecutionOrigin::Mixed
        },
        asset_market_country: sell.asset_market_country,
        source: source.to_string(),
    });
}

// Adds to totals to local state.
fn add_to_totals(totals: &mut TermTotals, pnl: f64) {
    if pnl >= 0.0 {
        totals.gains += pnl;
    } else {
        totals.losses += pnl;
    }
    totals.net += pnl;
    totals.count += 1;
}

// Groups realized P&L by execution origin for reporting.
fn origin_breakdown(positions: &[ClosedPosition]) -> Vec<serde_json::Value> {
    let mut by_origin: BTreeMap<String, TermTotals> = BTreeMap::new();
    for position in positions {
        let totals = by_origin
            .entry(position.execution_origin.as_str().to_string())
            .or_default();
        add_to_totals(totals, position.pnl);
    }
    by_origin
        .into_iter()
        .map(|(execution_origin, totals)| {
            serde_json::json!({
                "execution_origin": execution_origin,
                "gains": totals.gains,
                "losses": totals.losses,
                "net": totals.net,
                "positions": totals.count,
            })
        })
        .collect()
}

// Handles net taxable logic.
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

#[derive(Debug, Default)]
struct BrazilMonthlyBucket {
    normal_sales: f64,
    normal_net: f64,
    day_trade_net: f64,
}

#[derive(Debug, Default)]
struct BrazilTaxComputation {
    normal_taxable: f64,
    day_trade_taxable: f64,
    foreign_taxable: f64,
    normal_tax: f64,
    day_trade_tax: f64,
    foreign_tax: f64,
    loss_after_netting: f64,
}

// Returns whether a Brazilian-market disposal is a day trade.
fn is_brazil_day_trade(position: &ClosedPosition) -> bool {
    position.asset_market_country == compliance::TaxCountry::Br
        && position.entry_date == position.exit_date
}

// Returns whether a Brazilian-market disposal is a normal/swing trade.
fn is_brazil_normal_trade(position: &ClosedPosition) -> bool {
    position.asset_market_country == compliance::TaxCountry::Br
        && position.entry_date != position.exit_date
}

// Calculates Brazil individual equity tax using monthly B3 buckets and foreign fallback.
fn brazil_tax_computation(positions: &[&ClosedPosition]) -> BrazilTaxComputation {
    let mut months: BTreeMap<(i32, u32), BrazilMonthlyBucket> = BTreeMap::new();
    let mut foreign_net = 0.0;
    for position in positions {
        if position.asset_market_country == compliance::TaxCountry::Br {
            let key = (position.exit_date.year(), position.exit_date.month());
            let bucket = months.entry(key).or_default();
            if is_brazil_day_trade(position) {
                bucket.day_trade_net += position.pnl;
            } else {
                bucket.normal_sales += (position.exit_price * position.qty).max(0.0);
                bucket.normal_net += position.pnl;
            }
        } else {
            foreign_net += position.pnl;
        }
    }

    let mut result = BrazilTaxComputation::default();
    let mut normal_loss_carry = 0.0;
    let mut day_trade_loss_carry = 0.0;
    for month in months.values() {
        let day_adjusted = month.day_trade_net - day_trade_loss_carry;
        if day_adjusted > 0.0 {
            result.day_trade_taxable += day_adjusted;
            result.day_trade_tax += day_adjusted * 0.20;
            day_trade_loss_carry = 0.0;
        } else {
            day_trade_loss_carry = -day_adjusted;
        }

        if month.normal_sales <= 20_000.0 && month.normal_net > 0.0 {
            continue;
        }
        if month.normal_sales <= 20_000.0 && month.normal_net < 0.0 {
            continue;
        }
        let normal_adjusted = month.normal_net - normal_loss_carry;
        if normal_adjusted > 0.0 {
            result.normal_taxable += normal_adjusted;
            result.normal_tax += normal_adjusted * 0.15;
            normal_loss_carry = 0.0;
        } else {
            normal_loss_carry = -normal_adjusted;
        }
    }

    if foreign_net > 0.0 {
        result.foreign_taxable = foreign_net;
        result.foreign_tax = foreign_net * 0.15;
    } else {
        result.loss_after_netting += -foreign_net;
    }
    result.loss_after_netting += normal_loss_carry + day_trade_loss_carry;
    result
}

// Handles progressive tax logic.
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

// Handles marginal rate logic.
fn marginal_rate(income: f64, brackets: &[OrdinaryBracket]) -> f64 {
    let income = income.max(0.0);
    for bracket in brackets {
        if bracket.upper.map(|upper| income <= upper).unwrap_or(true) {
            return bracket.rate;
        }
    }
    brackets.last().map(|bracket| bracket.rate).unwrap_or(0.0)
}

// Handles niit tax for trading gains logic.
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

// Handles capital gain marginal rate logic.
fn capital_gain_marginal_rate(base_income: f64, brackets: &[TaxBracket]) -> f64 {
    marginal_rate(base_income, brackets)
}

// Handles capital gain tax logic.
fn capital_gain_tax(base_income: f64, gain: f64, brackets: &[TaxBracket]) -> f64 {
    if gain <= 0.0 {
        return 0.0;
    }
    progressive_tax(base_income.max(0.0) + gain, brackets) - progressive_tax(base_income, brackets)
}

// Formats a floating-point value in the selected reporting currency.
fn money_for(country: compliance::TaxCountry, value: f64) -> String {
    compliance::format_money_for(country, value)
}

// Formats a rate as a percentage.
fn pct(rate: f64) -> String {
    format!("{:.1}%", rate * 100.0)
}

// Handles term for position logic.
fn term_for_position(position: &ClosedPosition) -> &'static str {
    if (position.exit_date - position.entry_date).num_days() > 365 {
        "long"
    } else {
        "short"
    }
}

// Returns country-specific tax treatment for operation details.
fn tax_treatment_for_position(
    country: compliance::TaxCountry,
    position: &ClosedPosition,
) -> &'static str {
    match country {
        compliance::TaxCountry::Br if is_brazil_day_trade(position) => "br_day_trade",
        compliance::TaxCountry::Br if is_brazil_normal_trade(position) => "br_normal_swing",
        compliance::TaxCountry::Br => "br_foreign_financial_investment",
        compliance::TaxCountry::Sg => "sg_revenue_or_capital_fact_test",
        compliance::TaxCountry::Gb => "gb_share_cgt",
        compliance::TaxCountry::Us => {
            if term_for_position(position) == "long" {
                "us_long_term"
            } else {
                "us_short_term"
            }
        }
    }
}

// Handles operation tax impact logic.
fn operation_tax_impact(
    position: &ClosedPosition,
    table: &TaxYearTable,
    estimated_income: f64,
    all_positions: &[&ClosedPosition],
) -> f64 {
    match table.country {
        compliance::TaxCountry::Us => {
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
        compliance::TaxCountry::Br if is_brazil_day_trade(position) => position.pnl * 0.20,
        compliance::TaxCountry::Br if is_brazil_normal_trade(position) => {
            let month_sales = all_positions
                .iter()
                .filter(|candidate| {
                    is_brazil_normal_trade(candidate)
                        && candidate.exit_date.year() == position.exit_date.year()
                        && candidate.exit_date.month() == position.exit_date.month()
                })
                .map(|candidate| (candidate.exit_price * candidate.qty).max(0.0))
                .sum::<f64>();
            if month_sales <= 20_000.0 && position.pnl > 0.0 {
                0.0
            } else {
                position.pnl * 0.15
            }
        }
        compliance::TaxCountry::Br => position.pnl * 0.15,
        compliance::TaxCountry::Sg => {
            position.pnl * marginal_rate(estimated_income, &table.ordinary)
        }
        compliance::TaxCountry::Gb => {
            position.pnl * capital_gain_marginal_rate(estimated_income, &table.capital_gains)
        }
    }
}

// Builds quarter breakdown tax periods.
fn quarter_breakdown(
    country: compliance::TaxCountry,
    year: i32,
    account_filters: &[String],
) -> anyhow::Result<Vec<TaxEstimate>> {
    let mut estimates = Vec::new();
    for quarter in refreshable_quarters_for_date_for(country, year, Utc::now().date_naive()) {
        let (estimate, _, _, _, _, _, _, _) =
            build_estimates_with_filters(year, &[quarter], account_filters)?;
        estimates.push(estimate);
    }
    Ok(estimates)
}

// Handles refreshable quarters for date logic.
#[cfg(test)]
fn refreshable_quarters_for_date(year: i32, today: NaiveDate) -> Vec<u8> {
    refreshable_quarters_for_date_for(compliance::TaxCountry::Us, year, today)
}

// Handles refreshable quarters for date logic by country tax calendar.
fn refreshable_quarters_for_date_for(
    country: compliance::TaxCountry,
    year: i32,
    today: NaiveDate,
) -> Vec<u8> {
    (1..=4)
        .filter(|quarter| {
            tax_period_range_for(country, year, &[*quarter])
                .map(|(start, _, _, _)| start <= today)
                .unwrap_or(false)
        })
        .collect()
}

// Handles known tax accounts logic.
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

// Handles the tax accounts CLI action.
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

// Handles bracket rows logic.
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

// Handles the tax show brackets CLI action.
pub fn cmd_tax_show_brackets(year: i32, json: bool) -> anyhow::Result<()> {
    let app_config = config::load()?;
    let country = app_config
        .tax
        .residency_country
        .as_deref()
        .and_then(compliance::TaxCountry::parse)
        .unwrap_or(compliance::TaxCountry::Us);
    let filing_status = app_config.tax.filing_status.as_deref().unwrap_or("single");
    let filing_status = FilingStatus::parse(filing_status)?;
    let estimated_income = app_config.tax.estimated_annual_income.unwrap_or(0.0);
    let table = tax_table(year, filing_status, country)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "year": year,
                "tax_country_code": table.profile.country_code,
                "tax_country_name": table.profile.country_name,
                "currency_code": table.profile.currency_code,
                "currency_symbol": table.profile.currency_symbol,
                "tax_rule_name": table.profile.tax_rule_name,
                "tax_rule_summary": table.profile.tax_rule_summary,
                "tax_year_label": table.tax_year_label,
                "tax_period_basis": table.tax_period_basis,
                "taxable_gain_model": table.taxable_gain_model,
                "lot_matching_rule": table.lot_matching_rule,
                "rule_limitations": table.rule_limitations,
                "short_term_label": table.short_term_label,
                "long_term_label": table.long_term_label,
                "filing_status": filing_status.label(),
                "estimated_income": estimated_income,
                "bracket_config": table.source_label,
                "annual_exempt_amount": table.annual_exempt_amount,
                "tax_buckets": {
                    "first": {
                        "label": table.short_term_label,
                        "rates": bracket_rows(&table.ordinary),
                    },
                    "second": {
                        "label": table.long_term_label,
                        "rates": bracket_rows(&table.capital_gains),
                    }
                },
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

    println!(
        "Tax Rules - {} ({}) {}",
        table.profile.country_name, table.profile.country_code, year
    );
    println!("Reporting currency: {}", table.profile.currency_code);
    println!("Rule: {}", table.profile.tax_rule_name);
    println!("{}", table.profile.tax_rule_summary);
    println!("Tax year: {}", table.tax_year_label);
    println!("Period basis: {}", table.tax_period_basis);
    println!("Lot matching: {}", table.lot_matching_rule);
    println!(
        "Buckets: {} / {}",
        table.short_term_label, table.long_term_label
    );
    if country == compliance::TaxCountry::Us {
        println!("Filing status: {}", filing_status.label());
    }
    println!(
        "Estimated annual income: {}",
        money_for(country, estimated_income)
    );
    println!("Bracket config: {}", table.source_label);
    println!();
    if country == compliance::TaxCountry::Us {
        println!("Short-term capital gains: taxed as ordinary income.");
        println!("Ordinary income brackets:");
        let mut lower = 0.0;
        for bracket in &table.ordinary {
            let upper = bracket
                .upper
                .map(|value| money_for(country, value))
                .unwrap_or_else(|| "and above".to_string());
            println!(
                "  {} to {}: {}",
                money_for(country, lower),
                upper,
                pct(bracket.rate)
            );
            if let Some(upper) = bracket.upper {
                lower = upper;
            }
        }
        println!();
        println!("Long-term capital gains brackets:");
    } else {
        println!("Country rate table:");
        if table.annual_exempt_amount > 0.0 {
            println!(
                "  Annual exempt amount: {}",
                money_for(country, table.annual_exempt_amount)
            );
        }
    }
    let mut lower = 0.0;
    for bracket in &table.capital_gains {
        let upper = bracket
            .upper
            .map(|value| money_for(country, value))
            .unwrap_or_else(|| "and above".to_string());
        println!(
            "  {} to {}: {}",
            money_for(country, lower),
            upper,
            pct(bracket.rate)
        );
        if let Some(upper) = bracket.upper {
            lower = upper;
        }
    }
    if country == compliance::TaxCountry::Us {
        println!();
        println!(
            "Net Investment Income Tax: {} when income exceeds {}.",
            pct(table.niit_rate),
            money_for(country, table.niit_threshold)
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
    }
    Ok(())
}

// Calculates estimate for reporting or decisions.
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
        match table.country {
            compliance::TaxCountry::Br => {
                if is_brazil_day_trade(position) {
                    add_to_totals(&mut short, position.pnl);
                } else {
                    add_to_totals(&mut long, position.pnl);
                }
            }
            _ => {
                let held_days = (position.exit_date - position.entry_date).num_days();
                if held_days > 365 {
                    add_to_totals(&mut long, position.pnl);
                } else {
                    add_to_totals(&mut short, position.pnl);
                }
            }
        }
    }
    let total_net = short.net + long.net;
    let (
        netting,
        short_term_tax,
        long_term_tax,
        niit_tax,
        ordinary_marginal_rate,
        effective_short_rate,
        effective_long_rate,
        niit_rate,
    ) = match table.country {
        compliance::TaxCountry::Us => {
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
            (
                netting,
                short_term_tax,
                long_term_tax,
                niit_tax,
                ordinary_marginal_rate,
                effective_short_rate,
                effective_long_rate,
                niit_rate,
            )
        }
        compliance::TaxCountry::Br => {
            let br = brazil_tax_computation(positions);
            let short_term_tax = br.day_trade_tax;
            let long_term_tax = br.normal_tax + br.foreign_tax;
            let netting = TaxableNetting {
                taxable_short_term: br.day_trade_taxable,
                taxable_long_term: br.normal_taxable + br.foreign_taxable,
                capital_loss_after_netting: br.loss_after_netting,
            };
            let effective_short_rate = if br.day_trade_taxable > 0.0 {
                short_term_tax / br.day_trade_taxable
            } else {
                0.0
            };
            let effective_long_rate = if (br.normal_taxable + br.foreign_taxable) > 0.0 {
                long_term_tax / (br.normal_taxable + br.foreign_taxable)
            } else {
                0.0
            };
            (
                netting,
                short_term_tax,
                long_term_tax,
                0.0,
                0.15,
                effective_short_rate,
                effective_long_rate,
                0.0,
            )
        }
        compliance::TaxCountry::Sg => {
            let taxable_total = total_net.max(0.0);
            let ordinary_before = progressive_tax(estimated_income, &table.ordinary);
            let ordinary_after = progressive_tax(estimated_income + taxable_total, &table.ordinary);
            let total_tax = (ordinary_after - ordinary_before).max(0.0);
            let effective_rate = if taxable_total > 0.0 {
                total_tax / taxable_total
            } else {
                0.0
            };
            let netting = TaxableNetting {
                taxable_short_term: taxable_total,
                taxable_long_term: 0.0,
                capital_loss_after_netting: (-total_net).max(0.0),
            };
            (
                netting,
                total_tax,
                0.0,
                0.0,
                marginal_rate(estimated_income, &table.ordinary),
                effective_rate,
                0.0,
                0.0,
            )
        }
        compliance::TaxCountry::Gb => {
            let taxable_total = (total_net.max(0.0) - table.annual_exempt_amount).max(0.0);
            let total_tax = capital_gain_tax(estimated_income, taxable_total, &table.capital_gains);
            let effective_rate = if taxable_total > 0.0 {
                total_tax / taxable_total
            } else {
                0.0
            };
            let netting = TaxableNetting {
                taxable_short_term: taxable_total,
                taxable_long_term: 0.0,
                capital_loss_after_netting: (-total_net).max(0.0),
            };
            (
                netting,
                total_tax,
                0.0,
                0.0,
                capital_gain_marginal_rate(estimated_income, &table.capital_gains),
                effective_rate,
                0.0,
                0.0,
            )
        }
    };
    let total_tax = short_term_tax + long_term_tax + niit_tax;

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
        tax_country_code: table.profile.country_code.to_string(),
        tax_country_name: table.profile.country_name.to_string(),
        currency_code: table.profile.currency_code.to_string(),
        currency_symbol: table.profile.currency_symbol.to_string(),
        tax_rule_name: table.profile.tax_rule_name.to_string(),
        tax_rule_summary: table.profile.tax_rule_summary.to_string(),
        tax_year_label: table.tax_year_label.clone(),
        tax_period_basis: table.tax_period_basis.to_string(),
        taxable_gain_model: table.taxable_gain_model.to_string(),
        lot_matching_rule: table.lot_matching_rule.to_string(),
        rule_limitations: table
            .rule_limitations
            .iter()
            .map(|value| value.to_string())
            .collect(),
        short_term_label: table.short_term_label.to_string(),
        long_term_label: table.long_term_label.to_string(),
        filing_status,
        filing_status_label: filing_status.label().to_string(),
        estimated_annual_income: estimated_income,
        include_paper_accounts_for_estimate: include_paper,
        excluded_paper_positions,
        total_net,
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

// Builds estimates with filters from configured inputs.
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
    let country = app_config
        .tax
        .residency_country
        .as_deref()
        .and_then(compliance::TaxCountry::parse)
        .unwrap_or(compliance::TaxCountry::Us);
    let filing_status = match app_config.tax.filing_status.as_deref() {
        Some(value) => FilingStatus::parse(value)?,
        None if country == compliance::TaxCountry::Us => {
            anyhow::bail!(
                "tax.filing_status is not set in {}.",
                config::config_path().display()
            )
        }
        None => FilingStatus::Single,
    };
    let estimated_income = match app_config.tax.estimated_annual_income {
        Some(value) => value,
        None if country == compliance::TaxCountry::Us => {
            anyhow::bail!(
                "tax.estimated_annual_income is not set in {}.",
                config::config_path().display()
            )
        }
        None => 0.0,
    };
    let include_paper = app_config
        .tax
        .include_paper_accounts_for_estimate
        .unwrap_or(false)
        || account_filters_may_include_paper(&account_tokens);
    let table = tax_table(year, filing_status, country)?;
    let (start, end, quarter_id, period_label) = tax_period_range_for(country, year, quarters)?;
    let conn = open_db()?;
    let (mut positions, excluded_paper_positions) =
        load_closed_positions_for_country(&conn, start, end, include_paper, country)?;
    positions.retain(|position| position_matches_account_filters(position, &account_tokens));
    sort_closed_positions_newest_first(&mut positions);
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

// Builds estimates from configured inputs.
fn build_estimates(
    year: i32,
    quarters: &[u8],
) -> anyhow::Result<(TaxEstimate, Vec<TaxEstimate>, Vec<TaxEstimate>, usize)> {
    let (consolidated, providers, accounts, position_count, _, _, _, _) =
        build_estimates_with_filters(year, quarters, &[])?;
    Ok((consolidated, providers, accounts, position_count))
}

// Writes csv to disk or storage.
fn write_csv(estimates: &[TaxEstimate], year: i32, period_label: &str) -> anyhow::Result<PathBuf> {
    paths::ensure_runtime_dirs()?;
    let path = paths::data_dir().join(format!("tax_{}_{}.csv", year, period_label));
    let mut file = paths::create_private_file(&path)?;
    writeln!(
        file,
        "year,quarter,period_label,period_start,period_end,scope,provider,account_ref,account_mode,paper_account,tax_country_code,tax_country_name,currency_code,currency_symbol,tax_rule_name,tax_year_label,tax_period_basis,taxable_gain_model,lot_matching_rule,short_term_label,long_term_label,filing_status,estimated_annual_income,short_gains,short_losses,short_net,short_count,long_gains,long_losses,long_net,long_count,total_net,taxable_short_term,taxable_long_term,capital_loss_after_netting,ordinary_marginal_rate,short_term_effective_rate,long_term_effective_rate,niit_rate,short_term_with_niit_effective_rate,long_term_with_niit_effective_rate,estimated_short_term_tax,estimated_long_term_tax,estimated_niit_tax,estimated_total_tax,position_count,generated_at_utc"
    )?;
    for estimate in estimates {
        writeln!(
            file,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:?},{:.2},{:.2},{:.2},{:.2},{},{:.2},{:.2},{:.2},{},{:.2},{:.2},{:.2},{:.2},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.2},{:.2},{:.2},{:.2},{},{}",
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
            estimate.tax_country_code,
            estimate.tax_country_name,
            estimate.currency_code,
            estimate.currency_symbol,
            estimate.tax_rule_name,
            estimate.tax_year_label,
            estimate.tax_period_basis,
            estimate.taxable_gain_model,
            estimate.lot_matching_rule,
            estimate.short_term_label,
            estimate.long_term_label,
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

// Handles refresh current year estimates logic.
pub fn refresh_current_year_estimates() -> anyhow::Result<()> {
    let country = config::tax_residency_country();
    let year = match country {
        compliance::TaxCountry::Sg => Utc::now().year() + 1,
        compliance::TaxCountry::Gb => {
            let today = Utc::now().date_naive();
            if today
                >= NaiveDate::from_ymd_opt(today.year(), 4, 6).expect("valid UK tax year start")
            {
                today.year()
            } else {
                today.year() - 1
            }
        }
        _ => Utc::now().year(),
    };
    let today = Utc::now().date_naive();
    let mut estimates = Vec::new();
    let (consolidated, providers, accounts, _) = build_estimates(year, &[])?;
    estimates.push(consolidated);
    estimates.extend(providers);
    estimates.extend(accounts);
    for quarter in refreshable_quarters_for_date_for(country, year, today) {
        let quarters = [quarter];
        let (consolidated, providers, accounts, _) = build_estimates(year, &quarters)?;
        estimates.push(consolidated);
        estimates.extend(providers);
        estimates.extend(accounts);
    }
    save_estimates(&estimates)?;
    Ok(())
}

// Handles the tax show CLI action.
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
    let quarter_estimates = quarter_breakdown(table.country, year, &account_filters)?;
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
        let position_refs = positions.iter().collect::<Vec<_>>();
        let detail_rows = if details {
            positions
                .iter()
                .map(|position| {
                    let asset_profile = compliance::tax_country_profile(position.asset_market_country);
                    serde_json::json!({
                        "provider": position.provider,
                        "account_ref": position.account_ref,
                        "account_mode": position.account_mode,
                        "tax_universe": if position.paper_account { "paper" } else { "real" },
                        "symbol": position.symbol,
                        "term": term_for_position(position),
                        "tax_treatment": tax_treatment_for_position(table.country, position),
                        "asset_market_country_code": asset_profile.country_code,
                        "asset_market_country_name": asset_profile.country_name,
                        "qty": position.qty,
                        "entry_date": position.entry_date.to_string(),
                        "entry_price": position.entry_price,
                        "exit_date": position.exit_date.to_string(),
                        "exit_price": position.exit_price,
                        "pnl": position.pnl,
                        "entry_execution_origin": position.entry_execution_origin.as_str(),
                        "exit_execution_origin": position.exit_execution_origin.as_str(),
                        "execution_origin": position.execution_origin.as_str(),
                        "estimated_tax_impact": operation_tax_impact(position, &table, estimated_income, &position_refs),
                        "estimated_federal_tax_impact": operation_tax_impact(position, &table, estimated_income, &position_refs),
                        "currency_code": table.profile.currency_code,
                        "lot_matching_rule": table.lot_matching_rule,
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
                "by_execution_origin": origin_breakdown(&positions),
                "details": detail_rows,
                "account_filters": normalize_account_filters(&account_filters),
                "source_table": if table.country == compliance::TaxCountry::Gb {
                    "provider_fill_activities HMRC same-day/30-day/Section 104 matching when available"
                } else {
                    "auto_positions closed rows + provider_fill_activities FIFO fills"
                },
                "tax_database": tax_db_path(),
                "export_path": export_path,
                "tax_year_label": table.tax_year_label,
                "tax_period_basis": table.tax_period_basis,
                "taxable_gain_model": table.taxable_gain_model,
                "lot_matching_rule": table.lot_matching_rule,
                "rule_limitations": table.rule_limitations,
                "short_term_label": table.short_term_label,
                "long_term_label": table.long_term_label,
                "notes": [
                    table.profile.tax_rule_summary,
                    "Amounts are reported in the configured tax-residency currency; historical FX conversion is not modeled here.",
                    "This is an estimate, not tax advice."
                ],
            }))?
        );
        return Ok(());
    }

    println!(
        "Tax Estimate - {} ({}) {} {}",
        consolidated.tax_country_name,
        consolidated.tax_country_code,
        year,
        period_label.to_ascii_uppercase()
    );
    println!("Rule: {}", consolidated.tax_rule_name);
    println!("Currency: {}", consolidated.currency_code);
    println!("Tax year: {}", consolidated.tax_year_label);
    println!("Period basis: {}", consolidated.tax_period_basis);
    println!("Lot matching: {}", consolidated.lot_matching_rule);
    println!(
        "Period: {} to {}",
        consolidated.period_start, consolidated.period_end
    );
    if table.country == compliance::TaxCountry::Us {
        println!("Filing status: {}", consolidated.filing_status_label);
    }
    println!(
        "Estimated annual income: {}",
        money_for(table.country, consolidated.estimated_annual_income)
    );
    println!("Income assumption: taxable ordinary income before trading gains");
    println!(
        "Data source: {}",
        if table.country == compliance::TaxCountry::Gb {
            "provider_fill_activities HMRC share matching when available"
        } else {
            "auto_positions closed rows + provider_fill_activities FIFO fills"
        }
    );
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
        "{}: gains {} | losses {} | net {} | positions {}",
        consolidated.short_term_label,
        money_for(table.country, consolidated.short_term.gains),
        money_for(table.country, consolidated.short_term.losses),
        money_for(table.country, consolidated.short_term.net),
        consolidated.short_term.count
    );
    println!(
        "{}: gains {} | losses {} | net {} | positions {}",
        consolidated.long_term_label,
        money_for(table.country, consolidated.long_term.gains),
        money_for(table.country, consolidated.long_term.losses),
        money_for(table.country, consolidated.long_term.net),
        consolidated.long_term.count
    );
    println!(
        "Total realized net: {}",
        money_for(table.country, consolidated.total_net)
    );
    let by_origin = origin_breakdown(&positions);
    if !by_origin.is_empty() {
        println!();
        println!("Realized P&L by execution origin:");
        println!(
            "{:<18} {:>12} {:>12} {:>12} {:>10}",
            "Origin", "Gains", "Losses", "Net", "Positions"
        );
        println!("{}", "-".repeat(70));
        for row in &by_origin {
            println!(
                "{:<18} {:>12} {:>12} {:>12} {:>10}",
                row.get("execution_origin")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown"),
                money_for(
                    table.country,
                    row.get("gains")
                        .and_then(|value| value.as_f64())
                        .unwrap_or(0.0)
                ),
                money_for(
                    table.country,
                    row.get("losses")
                        .and_then(|value| value.as_f64())
                        .unwrap_or(0.0)
                ),
                money_for(
                    table.country,
                    row.get("net")
                        .and_then(|value| value.as_f64())
                        .unwrap_or(0.0)
                ),
                row.get("positions")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0)
            );
        }
    }
    println!();
    println!(
        "Taxable after netting: {} {} | {} {} | unused net capital loss {}",
        consolidated.short_term_label,
        money_for(
            table.country,
            consolidated.taxable_after_netting.taxable_short_term
        ),
        consolidated.long_term_label,
        money_for(
            table.country,
            consolidated.taxable_after_netting.taxable_long_term
        ),
        money_for(
            table.country,
            consolidated
                .taxable_after_netting
                .capital_loss_after_netting
        )
    );
    println!(
        "Rates: ordinary marginal {} | {} effective {} | {} effective {}",
        pct(consolidated.rates.ordinary_marginal_rate),
        consolidated.short_term_label,
        pct(consolidated.rates.short_term_effective_rate),
        consolidated.long_term_label,
        pct(consolidated.rates.long_term_effective_rate)
    );
    if table.country == compliance::TaxCountry::Us {
        println!(
            "Additional federal tax: Net Investment Income Tax {} effective, threshold {}",
            pct(consolidated.rates.niit_rate),
            money_for(table.country, table.niit_threshold)
        );
        println!(
            "Rates with NIIT: short {} | long {}",
            pct(consolidated.rates.short_term_with_niit_effective_rate),
            pct(consolidated.rates.long_term_with_niit_effective_rate)
        );
    }
    println!(
        "Estimated tax: {} {} | {} {} | additional {} | total {}",
        consolidated.short_term_label,
        money_for(table.country, consolidated.estimated_federal_tax.short_term),
        consolidated.long_term_label,
        money_for(table.country, consolidated.estimated_federal_tax.long_term),
        money_for(
            table.country,
            consolidated.estimated_federal_tax.net_investment_income_tax
        ),
        money_for(table.country, consolidated.estimated_federal_tax.total)
    );
    if !quarter_estimates.is_empty() {
        println!();
        println!("Quarter breakdown:");
        println!(
            "{:<8} {:>12} {:>12} {:>12} {:>12} {:>8}",
            "Quarter",
            consolidated.short_term_label,
            consolidated.long_term_label,
            "Total net",
            "Tax",
            "Positions"
        );
        println!("{}", "-".repeat(76));
        for estimate in &quarter_estimates {
            println!(
                "{:<8} {:>12} {:>12} {:>12} {:>12} {:>8}",
                estimate.period_label.to_ascii_uppercase(),
                money_for(table.country, estimate.short_term.net),
                money_for(table.country, estimate.long_term.net),
                money_for(table.country, estimate.total_net),
                money_for(table.country, estimate.estimated_federal_tax.total),
                estimate.position_count
            );
        }
    }
    if details {
        let position_refs = positions.iter().collect::<Vec<_>>();
        println!();
        println!("Operation details:");
        println!(
            "{:<10} {:<8} {:<6} {:<10} {:<8} {:<10} {:>10} {:<10} {:>10} {:>11} {:>11}",
            "Account",
            "Symbol",
            "Tax",
            "Origin",
            "Qty",
            "Entry",
            "Entry Px",
            "Exit",
            "Exit Px",
            "P&L",
            "Tax impact"
        );
        println!("{}", "-".repeat(128));
        for position in &positions {
            println!(
                "{:<10} {:<8} {:<6} {:<10} {:<8.2} {:<10} {:>10} {:<10} {:>10} {:>11} {:>11}",
                position.account_ref,
                position.symbol,
                tax_treatment_for_position(table.country, position),
                position.execution_origin.short_label(),
                position.qty,
                position.entry_date,
                money_for(table.country, position.entry_price),
                position.exit_date,
                money_for(table.country, position.exit_price),
                money_for(table.country, position.pnl),
                money_for(
                    table.country,
                    operation_tax_impact(position, &table, estimated_income, &position_refs)
                )
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
    println!("Note: estimate only. It is not tax advice and does not replace local filing review.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_closed_position(
        symbol: &str,
        entry_date: &str,
        exit_date: &str,
        qty: f64,
        exit_price: f64,
        pnl: f64,
        asset_market_country: compliance::TaxCountry,
    ) -> ClosedPosition {
        ClosedPosition {
            provider: "test".to_string(),
            account_ref: "default".to_string(),
            account_mode: "paper".to_string(),
            paper_account: true,
            symbol: symbol.to_string(),
            qty,
            entry_date: NaiveDate::parse_from_str(entry_date, "%Y-%m-%d").unwrap(),
            entry_price: exit_price - (pnl / qty),
            exit_date: NaiveDate::parse_from_str(exit_date, "%Y-%m-%d").unwrap(),
            exit_price,
            pnl,
            entry_execution_origin: origin::ExecutionOrigin::MlaiAuto,
            exit_execution_origin: origin::ExecutionOrigin::MlaiAuto,
            execution_origin: origin::ExecutionOrigin::MlaiAuto,
            asset_market_country,
            source: "test".to_string(),
        }
    }

    #[test]
    // Handles Brazil B3 monthly normal/day-trade rules plus foreign fallback.
    fn brazil_tax_uses_b3_monthly_rules_and_foreign_fallback() {
        let positions = vec![
            test_closed_position(
                "PETR4.SA",
                "2026-01-02",
                "2026-01-10",
                100.0,
                100.0,
                1_000.0,
                compliance::TaxCountry::Br,
            ),
            test_closed_position(
                "VALE3.SA",
                "2026-02-02",
                "2026-02-10",
                300.0,
                100.0,
                3_000.0,
                compliance::TaxCountry::Br,
            ),
            test_closed_position(
                "BBDC4.SA",
                "2026-02-12",
                "2026-02-12",
                100.0,
                50.0,
                1_000.0,
                compliance::TaxCountry::Br,
            ),
            test_closed_position(
                "AAPL",
                "2026-03-01",
                "2026-03-20",
                10.0,
                200.0,
                1_000.0,
                compliance::TaxCountry::Us,
            ),
        ];
        let refs = positions.iter().collect::<Vec<_>>();
        let tax = brazil_tax_computation(&refs);
        assert_eq!(tax.normal_taxable, 3_000.0);
        assert_eq!(tax.day_trade_taxable, 1_000.0);
        assert_eq!(tax.foreign_taxable, 1_000.0);
        assert_eq!(tax.normal_tax, 450.0);
        assert_eq!(tax.day_trade_tax, 200.0);
        assert_eq!(tax.foreign_tax, 150.0);
    }

    #[test]
    // Handles refreshable quarters skip future current year quarters logic.
    fn refreshable_quarters_skip_future_current_year_quarters() {
        let today = NaiveDate::from_ymd_opt(2026, 5, 3).unwrap();
        assert_eq!(refreshable_quarters_for_date(2026, today), vec![1, 2]);
    }

    #[test]
    // Handles refreshable quarters include all past year quarters logic.
    fn refreshable_quarters_include_all_past_year_quarters() {
        let today = NaiveDate::from_ymd_opt(2026, 5, 3).unwrap();
        assert_eq!(refreshable_quarters_for_date(2025, today), vec![1, 2, 3, 4]);
    }
}
