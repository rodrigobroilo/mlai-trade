# BR (ISO 3166-1 alpha-2: BR) Tax & Compliance Rules for Stock Trading - Comprehensive Reference

> **Country Code:** BR
> **Reporting Currency:** BRL (R$)
> **Last Updated:** June 30, 2026
> **Source References:** Receita Federal, B3, CVM, Banco Central, Lei 14.754/2023
> **Purpose:** Country-specific tax/compliance reference for the mlai-trade Brazil profile
> **Status:** Applies when `tax.residency_country` is `BR`
> **Disclaimer:** This is a research summary, NOT legal/tax advice. Consult a qualified Brazilian tax professional before filing or trading.

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

### Rules Every Brazil Tax Resident Trader MUST Know

| Rule | Summary | Risk |
| --- | --- | --- |
| **Country/currency** | `BR` uses BRL (`R$`) for buy/sell, wash/replacement views, tax brackets, thresholds, and exports. | Provider values in another currency need reliable FX conversion before filing. |
| **No short/long split for B3 stocks** | Brazilian individual stock gains on B3 are generally not taxed differently because a holding period is short or long. | US-style short/long assumptions are wrong for Brazil. |
| **Normal/swing B3 stock trades** | Monthly net gains from normal cash-market stock sales are taxed at 15%, subject to the monthly R$20,000 stock-sale exemption. | Exemption is narrow and does not cover day trades, ETFs, FIIs, options, or futures. |
| **Day trades** | Same-day buy and sell of the same asset are taxed separately at 20%. | Day-trade losses offset only day-trade gains. |
| **Monthly netting** | Domestic B3 results are calculated month by month and category by category. | Mixing normal, day-trade, FII, option, or futures buckets can produce wrong tax. |
| **Loss carryforward** | Losses can generally carry forward within the same tax category. | Prior-year losses and official controls are not modeled by mlai-trade. |
| **Foreign financial investments** | Lei 14.754/2023 generally taxes positive annual net income/gains from foreign financial investments at 15%. | Foreign broker data, FX evidence, and foreign tax credit analysis remain taxpayer work. |
| **No modeled wash-sale blocker** | Brazil does not have a US Section 1091-style loss disallowance modeled by this app. | Market manipulation and artificial trades are still illegal. |
| **Withholding tax (IRRF)** | Brokers may withhold "dedo-duro" tax: commonly 0.005% on normal sale proceeds and 1% on positive day-trade results. | IRRF credits are not fully modeled; reconcile broker notes before paying DARF. |
| **Paper trading** | Paper trading creates no tax events. | Simulation estimates are only planning data. |

### Source-Aligned Notes

1. `mlai-trade` treats `BR` as one selected tax residency country. It reports currency as BRL and does not mix rules with US/GB/SG profiles.
2. Brazilian-market detection uses provider exchange metadata, known B3/BOVESPA suffixes such as `.SA`, and provider names. Unknown symbols default to the selected tax country.
3. The built-in Brazil model covers individual cash-equity B3 normal/swing and day-trade calculations plus a simplified foreign financial investment fallback.
4. The built-in model does not replace official broker notes, DARF controls, prior-year loss carryforwards, IRRF credits, dividend/JCP treatment, ETFs, FIIs, options, futures, term markets, loans, lending, or official FX documentation.

### Things That Are ILLEGAL

- Insider trading and misuse of material non-public information.
- Market manipulation, artificial prices, spoofing, layering, pump-and-dump activity, or wash trading.
- False statements to brokers, exchanges, regulators, or tax authorities.
- Automated trading that violates broker, B3, CVM, or exchange access rules.

---

## 2. Tax Treatment of Trading Income

### B3 Normal/Swing Cash-Equity Trades

For individual taxpayers, ordinary cash-market stock disposals on B3 are generally calculated monthly:

| Item | Rule |
| --- | --- |
| Tax rate | 15% on positive monthly net gain. |
| Exemption | Monthly cash-market stock sales up to R$20,000 may exempt positive gains for that category. |
| Holding period | No separate short-term versus long-term stock CGT rate. |
| Losses | Losses generally carry forward to offset future gains in the same category. |
| Due date | DARF is generally due by the last business day of the month following the taxable month. |

The R$20,000 monthly exemption is narrow. It generally applies to normal cash-market stock sales by individuals and does not apply to day trades, ETFs, FIIs, options, futures, or other non-stock categories. `mlai-trade` applies this exemption only to normal Brazilian-market stock rows in the simplified model.

### B3 Day Trades

Day trades are taxed separately from normal/swing trades:

| Item | Rule |
| --- | --- |
| Tax rate | 20% on positive monthly net day-trade gain. |
| Exemption | No R$20,000 monthly stock-sale exemption. |
| Losses | Day-trade losses generally offset only day-trade gains. |
| Withholding | Brokers commonly withhold 1% of positive day-trade result as IRRF. |

### Foreign Financial Investments

For Brazilian tax residents, foreign brokerage investments are usually outside the B3 variable-income calculation. Under Lei 14.754/2023, positive annual net income/gains from foreign financial investments are generally taxed at 15% in a separate annual calculation.

`mlai-trade` uses a simplified annual 15% fallback for non-Brazilian market gains when the configured country is `BR`. It does not model foreign tax credits, treaties, official exchange-rate proof, account-by-account statements, offshore entities, trusts, or multi-year carryforward rules.

### Currency Treatment

The Brazil profile reports amounts in BRL. The app does not perform historical FX conversion. If provider fills are in USD, GBP, SGD, or another currency, convert with the official method and retain evidence before relying on exported totals for filing.

---

## 3. Day Trading Rules

Brazil does not use the US Pattern Day Trader framework. Day trading matters primarily because it creates a separate Brazilian tax bucket and may have broker, margin, suitability, and risk controls.

For Brazil tax calculations:

- A day trade is generally a buy and sell of the same asset on the same day.
- Day-trade gains are taxed at 20%.
- Day-trade losses are tracked separately from normal/swing losses.
- The R$20,000 monthly stock-sale exemption does not apply.

---

## 4. Wash Sale Rule

Brazil does not have a US Section 1091-style wash-sale rule modeled by `mlai-trade`. The Brazil profile therefore does not block replacement buys after a realized loss.

This does not allow abusive trading. Artificial transactions, wash trading to create false volume, market manipulation, and sham transactions can still violate CVM/B3 rules and tax anti-abuse principles.

---

## 5. Tax-Loss Harvesting

Brazilian tax-loss planning is category-specific:

| Category | General treatment |
| --- | --- |
| Normal/swing cash-equity losses | Offset future taxable normal/swing gains. |
| Day-trade losses | Offset future day-trade gains. |
| Foreign financial investment losses | Fact-specific under the foreign annual regime; not fully modeled. |
| Exempt-month stock activity | `mlai-trade` does not use gains or losses from monthly normal stock sales at or below R$20,000 in its simplified bucket. |

Keep broker notes, monthly worksheets, DARF receipts, and prior-year loss records. `mlai-trade` cannot infer official carryforwards that predate local trade history.

---

## 6. Options Trading Tax Rules

B3 options are not the same as cash-market stock disposals. Tax treatment depends on whether the option is opened, closed, exercised, expires, or is part of a day trade.

Common individual-tax considerations:

- Normal option operations are commonly taxed at 15% on monthly net gain.
- Day-trade option operations are commonly taxed at 20%.
- The R$20,000 monthly cash-stock exemption generally does not apply to options.
- Exercise can change the cost basis or proceeds of the underlying position.
- Broker notes and official monthly worksheets are required for accurate filing.

`mlai-trade` does not currently model Brazil options taxation.

---

## 7. Crypto Trading Tax Rules

Crypto is outside the stock-trading model. Brazilian crypto reporting and taxation can involve Receita Federal reporting obligations, exchange/wallet records, cost basis, monthly exemption rules, and foreign-asset treatment depending on custody and facts.

Do not use the B3 stock model for crypto. Keep separate records and consult current Receita Federal guidance.

---

## 8. Reporting Requirements

Brazilian tax residents commonly need:

| Requirement | Notes |
| --- | --- |
| Broker notes | Primary evidence for B3 trades, fees, IRRF, and monthly calculations. |
| Monthly DARF | Taxable monthly B3 gains are generally paid by the last business day of the following month. |
| Annual DIRPF | Positions, income, gains/losses, and taxes paid may need annual reporting. |
| Foreign assets | Foreign brokerage assets and income may require annual reporting and official FX support. |
| Banco Central CBE | High-value foreign assets may trigger Banco Central declaration obligations. |

`mlai-trade` exports estimates; it does not file DARF, DIRPF, CBE, or foreign investment statements.

---

## 9. Trader Tax Status

Brazil does not have a direct equivalent to US Section 475 trader tax status for this app's individual profile. Individuals usually calculate variable-income investment activity under Receita Federal rules. Legal entities, professional trading businesses, and offshore structures can have materially different tax regimes.

`mlai-trade` models an individual taxpayer profile only.

---

## 10. Prohibited Activities (ILLEGAL)

Do not use automated or manual trading to:

- Trade on insider information.
- Manipulate prices, liquidity, volume, opening/closing auctions, or order-book signals.
- Spoof, layer, or submit orders without genuine trading intent.
- Create artificial losses, volume, or sham transactions.
- Evade broker, exchange, CVM, Receita Federal, or Banco Central rules.

---

## 11. Algorithmic/Automated Trading Rules

Personal automation still needs complete records:

- Strategy version and parameters.
- Order submission, cancel, fill, and rejection history.
- Broker notes and account statements.
- Risk controls, position limits, and margin records.
- Evidence explaining why each trade occurred.

Broker and exchange rules may restrict API access, latency-sensitive activity, market-making, or high-frequency strategies. `mlai-trade` logs should be retained with tax records.

---

## 12. Foreign Account Considerations

Brazilian tax residents using foreign brokers should separately track:

- Official BRL conversion methodology and evidence.
- Foreign account balances and asset inventory.
- Dividends, interest, withholding taxes, and foreign tax credits.
- Annual foreign financial investment gains/losses under the applicable law.
- Banco Central reporting thresholds.

The app's foreign fallback is intentionally simplified and should be treated as a planning estimate.

---

## 13. Compliance Checklist

- Set `tax.residency_country` to `BR`.
- Confirm reported currency is `BRL`.
- Verify B3 symbols/exchanges are detected as Brazilian-market instruments.
- Reconcile every estimate against broker notes.
- Keep separate monthly normal/swing and day-trade worksheets.
- Track IRRF credits and DARF payments outside the app.
- Maintain prior-year loss carryforwards separately.
- Keep FX evidence for foreign broker activity.
- Review annual DIRPF and any Banco Central reporting obligations.

---

## 14. Tax Calendar

| Timing | Action |
| --- | --- |
| Every trade date | Save broker/order/fill evidence. |
| Monthly | Calculate B3 normal/swing and day-trade buckets. |
| Last business day of following month | Pay DARF for taxable monthly B3 gains when due. |
| Annually | Prepare DIRPF and foreign financial investment calculations. |
| When foreign assets exceed thresholds | Review Banco Central CBE obligations. |

---

## 15. Recommendations for Our Alpaca Setup

For a Brazil tax resident:

1. Configure exactly one tax country: `tax.residency_country = "BR"`.
2. Treat all estimates as BRL. If Alpaca activity is USD-denominated, maintain official BRL conversion records outside the app.
3. Use `mlai-trade compliance tax --show-brackets --year YYYY` to confirm the active Brazil profile and limitations.
4. Use `--details` to review inferred domestic/foreign market classification and operation tax treatment.
5. Do not rely on the app for IRRF credits, DARF generation, prior-year carryforwards, B3 options/futures/FIIs/ETFs, or final filings.
