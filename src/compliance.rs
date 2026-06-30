// Shared compliance constants and guardrails.
//
// Regulatory/statutory floors live here as code constants. Config may add
// safety buffers, but it must not reduce these floors.
//
// Function map:
// - tax_country_profile(): returns country/currency/rule metadata.
// - wash_sale_safety_buffer_days_for(): clamps user buffers to country floors.
// - wash_sale_forward_block_days_for(): returns the country replacement window.

use serde::Serialize;

/// IRC §1091 wash-sale replacement window after a loss sale.
pub const IRS_WASH_SALE_WINDOW_DAYS: i64 = 30;

/// Extra default buffer so we stay conservative around date/time boundaries.
pub const DEFAULT_WASH_SALE_SAFETY_BUFFER_DAYS: i64 = 1;

/// The user can increase this buffer, but not configure it below this floor.
pub const MIN_WASH_SALE_SAFETY_BUFFER_DAYS: i64 = DEFAULT_WASH_SALE_SAFETY_BUFFER_DAYS;

/// Conservative maximum day trades before this system refuses another same-day exit.
///
/// Classic PDT designation historically triggers on 4+ day trades in 5 business days,
/// so keeping this at 3 avoids intentionally entering the trigger event.
pub const PDT_TRADE_LIMIT: i64 = 3;

/// FINRA classic PDT minimum-equity threshold before the June 4, 2026 transition.
pub const PDT_MIN_EQUITY_DOLLARS_PRE_2026_06_04: f64 = 25_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TaxCountry {
    Us,
    Br,
    Sg,
    Gb,
}

impl TaxCountry {
    // Parses ISO 3166-1 alpha-2 country codes supported by this app.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "US" => Some(Self::Us),
            "BR" => Some(Self::Br),
            "SG" => Some(Self::Sg),
            "GB" | "UK" => Some(Self::Gb),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct TaxCountryProfile {
    pub country_code: &'static str,
    pub country_name: &'static str,
    pub currency_code: &'static str,
    pub currency_symbol: &'static str,
    pub wash_rule_name: &'static str,
    pub wash_rule_summary: &'static str,
    pub wash_sale_window_days: i64,
    pub wash_sale_default_buffer_days: i64,
    pub wash_sale_min_buffer_days: i64,
    pub wash_sale_blocking_enabled: bool,
    pub tax_rule_name: &'static str,
    pub tax_rule_summary: &'static str,
    pub tax_period_basis: &'static str,
    pub taxable_gain_model: &'static str,
    pub lot_matching_rule: &'static str,
    pub rule_limitations: &'static [&'static str],
}

// Returns country/currency/rule metadata for the configured tax residency.
pub fn tax_country_profile(country: TaxCountry) -> TaxCountryProfile {
    match country {
        TaxCountry::Us => TaxCountryProfile {
            country_code: "US",
            country_name: "United States",
            currency_code: "USD",
            currency_symbol: "$",
            wash_rule_name: "IRS wash sale",
            wash_rule_summary:
                "IRC Section 1091 replacement purchases within 30 days can disallow a loss deduction.",
            wash_sale_window_days: IRS_WASH_SALE_WINDOW_DAYS,
            wash_sale_default_buffer_days: DEFAULT_WASH_SALE_SAFETY_BUFFER_DAYS,
            wash_sale_min_buffer_days: MIN_WASH_SALE_SAFETY_BUFFER_DAYS,
            wash_sale_blocking_enabled: true,
            tax_rule_name: "US federal capital gains estimate",
            tax_rule_summary:
                "Short-term gains are estimated as ordinary income; long-term gains use IRS capital-gain brackets plus NIIT when applicable.",
            tax_period_basis: "calendar_year",
            taxable_gain_model: "short_long_capital_gains_plus_niit",
            lot_matching_rule: "fifo_realized_lots_from_local_or_provider_history",
            rule_limitations: &[
                "State taxes, local taxes, foreign tax credits, AMT, qualified dividends, broker-specific covered-lot elections, and full Form 8949 adjustments are not modeled.",
                "Wash-sale prevention blocks replacement buys but the tax estimate does not retroactively adjust basis for historical wash sales.",
            ],
        },
        TaxCountry::Br => TaxCountryProfile {
            country_code: "BR",
            country_name: "Brazil",
            currency_code: "BRL",
            currency_symbol: "R$",
            wash_rule_name: "No modeled wash-sale blocker",
            wash_rule_summary:
                "No US-style wash-sale disallowance is modeled for Brazilian tax residency.",
            wash_sale_window_days: 0,
            wash_sale_default_buffer_days: 0,
            wash_sale_min_buffer_days: 0,
            wash_sale_blocking_enabled: false,
            tax_rule_name: "Brazil equity income tax estimate",
            tax_rule_summary:
                "Brazilian-market stock disposals use monthly B3 variable-income rules: normal/swing trades at 15%, day trades at 20%, and the R$20,000 monthly stock-sale exemption for normal cash-market stock sales. Foreign financial investments fall back to annual 15% net taxation under Lei 14.754/2023.",
            tax_period_basis: "calendar_year",
            taxable_gain_model: "br_b3_monthly_day_trade_normal_netting_with_foreign_15_percent_fallback",
            lot_matching_rule: "fifo_realized_lots_from_local_or_provider_history",
            rule_limitations: &[
                "Brazilian domestic-market detection uses provider exchange metadata, known B3/BOVESPA suffixes, and provider names; unknown symbols default to the selected tax country.",
                "The B3 model covers individual stock cash-market normal/swing and day-trade taxation. ETFs, FIIs, options, futures, term markets, dividends/JCP, IRRF credits, DARF due dates, prior-year loss carryforwards, and official FX conversion evidence are not modeled.",
                "Foreign tax credits, treaty/reciprocity limits, and multi-year Lei 14.754 loss carryforwards are not modeled.",
            ],
        },
        TaxCountry::Sg => TaxCountryProfile {
            country_code: "SG",
            country_name: "Singapore",
            currency_code: "SGD",
            currency_symbol: "S$",
            wash_rule_name: "No modeled wash-sale blocker",
            wash_rule_summary:
                "No capital-gains wash-sale disallowance is modeled for Singapore tax residency.",
            wash_sale_window_days: 0,
            wash_sale_default_buffer_days: 0,
            wash_sale_min_buffer_days: 0,
            wash_sale_blocking_enabled: false,
            tax_rule_name: "Singapore trading income estimate",
            tax_rule_summary:
                "Singapore has no capital gains tax, but gains that are revenue or trading income are modeled with resident individual income tax rates.",
            tax_period_basis: "year_of_assessment_previous_calendar_year",
            taxable_gain_model: "capital_vs_revenue_test_with_conservative_revenue_income_estimate",
            lot_matching_rule: "fifo_realized_lots_from_local_or_provider_history",
            rule_limitations: &[
                "Singapore has no capital gains tax, but whether gains are capital or revenue/trading income is fact-specific; this app conservatively estimates revenue-income tax for realized trading activity.",
                "Personal reliefs, rebates, non-resident rates, not-ordinarily-resident legacy cases, and foreign income exceptions are not modeled.",
                "No statutory short-term versus long-term capital-gain split is modeled for Singapore.",
            ],
        },
        TaxCountry::Gb => TaxCountryProfile {
            country_code: "GB",
            country_name: "United Kingdom",
            currency_code: "GBP",
            currency_symbol: "£",
            wash_rule_name: "HMRC share matching",
            wash_rule_summary:
                "Same-day and 30-day bed-and-breakfasting share identification rules are modeled as a conservative 30-day replacement guard.",
            wash_sale_window_days: 30,
            wash_sale_default_buffer_days: 0,
            wash_sale_min_buffer_days: 0,
            wash_sale_blocking_enabled: true,
            tax_rule_name: "UK CGT estimate",
            tax_rule_summary:
                "Share gains use HMRC same-day, 30-day, and Section 104 matching with the annual exempt amount and applicable share CGT rates.",
            tax_period_basis: "uk_tax_year_6_april_to_5_april",
            taxable_gain_model: "share_cgt_after_hmrc_identification_and_annual_exempt_amount",
            lot_matching_rule: "same_day_then_30_day_then_section_104_pool",
            rule_limitations: &[
                "HMRC same-day, 30-day, and Section 104 matching is modeled from synced provider fills when available; missing historical fills can make the Section 104 pool incomplete.",
                "UK residency split-year rules, remittance basis, foreign tax credits, carried-forward losses from prior tax years, spouse transfers, reliefs, and non-share assets are not modeled.",
                "No statutory short-term versus long-term share CGT split is modeled for the United Kingdom.",
            ],
        },
    }
}

// Formats a monetary amount in the reporting currency for a tax country.
pub fn format_money_for(country: TaxCountry, value: f64) -> String {
    let profile = tax_country_profile(country);
    if value < 0.0 {
        format!("-{}{:.2}", profile.currency_symbol, value.abs())
    } else {
        format!("{}{:.2}", profile.currency_symbol, value)
    }
}

// Handles wash sale safety buffer days logic for a tax country.
pub fn wash_sale_safety_buffer_days_for(country: TaxCountry, configured: Option<i64>) -> i64 {
    let profile = tax_country_profile(country);
    if !profile.wash_sale_blocking_enabled {
        return 0;
    }
    configured
        .unwrap_or(profile.wash_sale_default_buffer_days)
        .max(profile.wash_sale_min_buffer_days)
}

// Handles wash sale forward block days logic for a tax country.
pub fn wash_sale_forward_block_days_for(
    country: TaxCountry,
    configured_buffer: Option<i64>,
) -> i64 {
    let profile = tax_country_profile(country);
    if !profile.wash_sale_blocking_enabled {
        return 0;
    }
    profile.wash_sale_window_days + wash_sale_safety_buffer_days_for(country, configured_buffer)
}
