# SG (ISO 3166-1 alpha-2: SG) Tax & Compliance Rules for Stock Trading - Comprehensive Reference

> **Country Code:** SG
> **Reporting Currency:** SGD (S$)
> **Last Updated:** June 30, 2026
> **Source References:** Inland Revenue Authority of Singapore (IRAS), Monetary Authority of Singapore (MAS), Singapore Exchange (SGX)
> **Purpose:** Country-specific tax/compliance reference for the mlai-trade Singapore profile
> **Status:** Applies when `tax.residency_country` is `SG`
> **Disclaimer:** This is a research summary, NOT legal/tax advice. Consult a qualified Singapore tax professional before filing or trading.

---

## Table of Contents

1. [Executive Summary - Must-Know Rules](#1-executive-summary)
2. [Tax Treatment of Trading Income](#2-tax-treatment-of-trading-income)
3. [Day Trading Rules](#3-day-trading-rules)
4. [Wash Sale Rule](#4-wash-sale-rule)
5. [Tax-Loss Harvesting](#5-tax-loss-harvesting)
6. [Options Trading Tax Rules](#6-options-trading-tax-rules)
7. [Crypto Trading Tax Rules](#7-crypto-trading-tax-rules)
8. [Reporting Requirements](#8-reporting-requirements)
9. [Trader Tax Status](#9-trader-tax-status)
10. [Prohibited Activities (ILLEGAL)](#10-prohibited-activities-illegal)
11. [Algorithmic/Automated Trading Rules](#11-algorithmicautomated-trading-rules)
12. [Foreign Account Considerations](#12-foreign-account-considerations)
13. [Compliance Checklist](#13-compliance-checklist)
14. [Tax Calendar](#14-tax-calendar)
15. [Recommendations for Our Alpaca Setup](#15-recommendations-for-our-alpaca-setup)

---

## 1. Executive Summary

### Rules Every Singapore Tax Resident Trader MUST Know

| Rule | Summary | Risk |
| --- | --- | --- |
| **Country/currency** | `SG` uses SGD (`S$`) for buy/sell, wash/replacement views, tax brackets, thresholds, and exports. | Provider values in another currency need reliable FX conversion before filing. |
| **No capital gains tax by default** | Singapore generally does not tax capital gains. | Gains can still be taxable if they are revenue or trading income. |
| **Capital vs revenue test** | IRAS looks at facts such as motive, frequency, holding period, financing, organization, and whether the activity is a trade. | Frequent systematic trading may be treated as taxable income. |
| **No statutory short/long CGT split** | Singapore does not have a US-style short-term/long-term capital-gain rate split. | Holding period is evidence in the fact test, not a separate rate table. |
| **No modeled wash-sale blocker** | There is no Singapore capital-gains wash-sale disallowance modeled by this app. | Artificial or sham trades can still be challenged. |
| **Year of Assessment basis** | Singapore tax Year of Assessment generally taxes income from the preceding calendar year. | Calendar-year exports must be mapped to the correct YA. |
| **Resident individual rates** | If gains are treated as trading/revenue income, resident individual progressive rates apply. | Personal reliefs, rebates, and non-resident rates are not modeled. |
| **Paper trading** | Paper trading creates no tax events. | Simulation estimates are only planning data. |

### Source-Aligned Notes

1. `mlai-trade` treats `SG` as one selected tax residency country. It reports currency as SGD and does not mix Singapore rules with US/BR/GB profiles.
2. The built-in Singapore model conservatively estimates realized trading gains as revenue income using resident individual tax rates.
3. Singapore has no general capital gains tax, so a true capital disposal should usually have no income-tax estimate.
4. Whether a trade is capital or revenue is fact-specific. The app cannot make a legal determination from fills alone.
5. Personal reliefs, rebates, non-resident rates, partnership rules, GST, foreign income exceptions, and business expense deductibility are not modeled.

### Things That Are ILLEGAL

- Insider trading and market misconduct.
- False trading, market manipulation, spoofing, layering, or creating misleading market appearance.
- Misleading statements to brokers, MAS, SGX, IRAS, or other authorities.
- Automated activity that violates broker, exchange, or market access rules.

---

## 2. Tax Treatment of Trading Income

### Capital Gains

Singapore generally does not tax capital gains. Gains from selling shares held as investments are therefore generally outside income tax.

The difficulty is classification. IRAS may treat gains as taxable revenue if the facts show the taxpayer is carrying on a trade or business, or if the transaction is an adventure in the nature of trade.

Common factual indicators include:

| Factor | Capital-leaning facts | Revenue/trading-leaning facts |
| --- | --- | --- |
| Intention | Investment, income, long-term holding | Profit from resale or market movement |
| Frequency | Occasional transactions | Repeated, systematic, high-volume transactions |
| Holding period | Longer holding periods | Very short holding periods |
| Financing | Own surplus capital | Borrowing or margin used to trade |
| Organization | Passive portfolio | Time, systems, records, strategy, business-like setup |
| Asset nature | Long-term investment asset | Asset commonly bought for resale |

`mlai-trade` cannot decide this legal question. Its Singapore estimate is conservative: it models realized trading gains as revenue income and labels the limitation.

### Resident Individual Income Tax Rates

When gains are taxable as income, `mlai-trade` uses resident individual progressive rates:

| Chargeable income band (SGD) | Marginal rate |
| --- | --- |
| 0 - 20,000 | 0% |
| 20,001 - 30,000 | 2% |
| 30,001 - 40,000 | 3.5% |
| 40,001 - 80,000 | 7% |
| 80,001 - 120,000 | 11.5% |
| 120,001 - 160,000 | 15% |
| 160,001 - 200,000 | 18% |
| 200,001 - 240,000 | 19% |
| 240,001 - 280,000 | 19.5% |
| 280,001 - 320,000 | 20% |
| 320,001 - 500,000 | 22% |
| 500,001 - 1,000,000 | 23% |
| Above 1,000,000 | 24% |

The app uses `tax.estimated_annual_income` as the base income for estimating incremental tax on realized trading/revenue gains.

### Currency Treatment

The Singapore profile reports amounts in SGD. The app does not perform historical FX conversion. If provider fills are in USD, GBP, BRL, or another currency, convert and retain evidence before relying on exported totals for filing.

---

## 3. Day Trading Rules

Singapore does not use the US Pattern Day Trader framework. Day trading matters because frequent, organized, short-horizon activity can support a conclusion that gains are revenue or trading income.

For Singapore:

- There is no special statutory day-trade capital-gain rate.
- Same-day trades may be evidence in the capital-versus-revenue analysis.
- A trading business may have taxable profits and potentially deductible expenses/losses subject to IRAS rules.
- Broker, margin, MAS, and SGX conduct rules still apply.

---

## 4. Wash Sale Rule

Singapore has no modeled capital-gains wash-sale disallowance in `mlai-trade`. The Singapore profile does not block replacement buys after a loss.

This does not permit sham transactions. Artificial arrangements can still be challenged under general anti-avoidance rules, and false trading or misleading market activity can breach securities laws.

---

## 5. Tax-Loss Harvesting

Tax-loss harvesting has limited relevance if a position is a capital investment because capital losses are generally not deductible against taxable income.

If the taxpayer is carrying on a trading business:

- Trading losses may be deductible or available for relief subject to IRAS rules.
- Losses must be supported by business records.
- Expense deductibility and loss relief are fact-specific.

`mlai-trade` does not model personal reliefs, business expenses, loss relief claims, or final capital/revenue classification.

---

## 6. Options Trading Tax Rules

Singapore options treatment also depends on capital-versus-revenue classification:

- Investment/capital option gains are generally not taxed as capital gains.
- Options traded as part of a business or adventure in the nature of trade can produce taxable income.
- Premiums, exercise, expiry, assignment, hedging purpose, and accounting treatment matter.

`mlai-trade` does not currently model option-specific Singapore tax rules.

---

## 7. Crypto Trading Tax Rules

Digital assets are outside the stock-trading model. Singapore generally applies the same capital-versus-revenue distinction: capital gains are not taxed, but gains from trading as a business or revenue activity can be taxable.

Goods and Services Tax, payment-token rules, business accounting, custody, and source of income can be fact-specific. Do not use the stock profile as a crypto tax engine.

---

## 8. Reporting Requirements

Singapore tax residents commonly need:

| Requirement | Notes |
| --- | --- |
| Annual filing | Report taxable income for the relevant Year of Assessment when filing is required. |
| YA basis | YA generally follows income from the preceding calendar year. |
| Records | Keep broker statements, order/fill logs, FX records, and reasoning for capital/revenue classification. |
| Trading business | If activity is a trade/business, maintain business-style accounts and expense support. |
| Capital investment | Capital gains are generally not reported as taxable income, but keep records supporting the position. |

`mlai-trade` exports estimates; it does not file IRAS returns.

---

## 9. Trader Tax Status

Singapore does not have an elective equivalent to US Section 475 trader tax status. Treatment turns on facts: whether gains are capital or revenue/trading income.

The app's Singapore profile is conservative for planning because it estimates revenue-income tax on realized trading gains. The taxpayer must decide, with professional advice if needed, whether activity is capital investment or taxable trading activity.

---

## 10. Prohibited Activities (ILLEGAL)

Do not use automated or manual trading to:

- Trade on inside information.
- Manipulate prices, liquidity, volume, or market signals.
- Spoof, layer, or create a false or misleading market appearance.
- Submit false statements to brokers, MAS, SGX, IRAS, or counterparties.
- Circumvent broker, margin, exchange, or market access controls.

---

## 11. Algorithmic/Automated Trading Rules

Personal automation should keep enough evidence to explain every order:

- Strategy version and parameters.
- Signal inputs.
- Order submission, cancel, fill, and rejection logs.
- Broker statements.
- Risk limits and account controls.
- Capital-versus-revenue classification rationale.

Broker, MAS, and SGX rules may restrict automated, high-frequency, market-making, or market-access activity. Keep `mlai-trade` logs with tax records.

---

## 12. Foreign Account Considerations

Singapore tax residents using foreign brokers should separately track:

- FX conversion into SGD.
- Foreign withholding taxes.
- Foreign dividends, interest, and other income.
- Whether income is Singapore-sourced, foreign-sourced, or received in Singapore.
- Whether foreign-sourced income exemptions apply.

Foreign-sourced income rules are fact-specific, especially for business/trading income and partnerships. The app does not model foreign income exemptions or foreign tax credits.

---

## 13. Compliance Checklist

- Set `tax.residency_country` to `SG`.
- Confirm reported currency is `SGD`.
- Decide whether activity is capital investment or revenue/trading income.
- Keep written support for that classification.
- Maintain broker, FX, and strategy records.
- Review resident/non-resident status and personal relief assumptions.
- Use app estimates only as planning data.

---

## 14. Tax Calendar

| Timing | Action |
| --- | --- |
| Every trade date | Save order/fill evidence and FX assumptions. |
| Year end | Classify realized activity as capital or revenue/trading income. |
| Year of Assessment | Prepare taxable income reporting for the preceding calendar year when filing is required. |
| On IRAS notice/deadline | File and pay by the applicable deadline. |

---

## 15. Recommendations for Our Alpaca Setup

For a Singapore tax resident:

1. Configure exactly one tax country: `tax.residency_country = "SG"`.
2. Treat all estimates as SGD. If Alpaca activity is USD-denominated, maintain SGD conversion records outside the app.
3. Use `mlai-trade compliance tax --show-brackets --year YYYY` to confirm the active Singapore profile and limitations.
4. Use `tax.estimated_annual_income` to approximate incremental resident income tax only if gains are revenue/trading income.
5. Do not treat the conservative revenue-income estimate as a final IRAS classification.
