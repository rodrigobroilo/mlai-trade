# GB (ISO 3166-1 alpha-2: GB) Tax & Compliance Rules for Stock Trading - Comprehensive Reference

> **Country Code:** GB (`UK` is accepted by config as an alias and normalized to `GB`)
> **Reporting Currency:** GBP
> **Last Updated:** June 30, 2026
> **Source References:** HM Revenue & Customs (HMRC), Financial Conduct Authority (FCA), London Stock Exchange rules
> **Purpose:** Country-specific tax/compliance reference for the mlai-trade United Kingdom profile
> **Status:** Applies when `tax.residency_country` is `GB`
> **Disclaimer:** This is a research summary, NOT legal/tax advice. Consult a qualified UK tax professional before filing or trading.

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

### Rules Every UK Tax Resident Trader MUST Know

| Rule | Summary | Risk |
| --- | --- | --- |
| **Country/currency** | `GB` uses GBP for buy/sell, wash/replacement views, tax brackets, thresholds, and exports. | Provider values in another currency need reliable FX conversion before filing. |
| **Tax year** | UK tax year runs from 6 April to 5 April. | Calendar-year reports do not match HMRC periods. |
| **No short/long share CGT split** | UK share CGT does not use a US-style one-year short/long split. | Holding period does not select a lower CGT rate. |
| **Share matching** | HMRC identifies shares using same-day matching, then 30-day "bed and breakfasting", then the Section 104 pool. | FIFO can be wrong for UK CGT. |
| **30-day matching is not a buy ban** | A later acquisition within 30 days changes which shares are matched to the disposal. | `mlai-trade` models this as a conservative replacement guard for compliance alerts. |
| **Annual exempt amount** | The app applies the annual exempt amount for individual share CGT where available. | Prior losses, spouse transfers, ISA/SIPP status, and reliefs are not modeled. |
| **Share CGT rates** | The app uses 10%/20% before tax year 2024 and 18%/24% from tax year 2024 onward. | Final rates depend on law, taxable income, and disposal date rules. |
| **Paper trading** | Paper trading creates no tax events. | Simulation estimates are only planning data. |

### Source-Aligned Notes

1. `mlai-trade` treats `GB` as one selected tax residency country. It reports currency as GBP and does not mix UK rules with US/BR/SG profiles.
2. The app uses the UK 6 April to 5 April tax year.
3. Provider fills are matched using HMRC ordering: same-day, then 30-day acquisitions, then Section 104 pool.
4. Missing provider fill history can make the Section 104 pool incomplete.
5. The app does not model ISA/SIPP tax shelters, remittance basis, split-year residence, foreign tax credits, prior-year carried-forward losses, spouse/civil partner transfers, or non-share assets.

### Things That Are ILLEGAL

- Insider dealing and market abuse.
- Market manipulation, spoofing, layering, pump-and-dump activity, or false/misleading orders.
- False statements to brokers, HMRC, FCA, exchanges, or counterparties.
- Automated activity that violates broker, venue, FCA, or exchange access rules.

---

## 2. Tax Treatment of Trading Income

### Share Capital Gains Tax

UK individual share disposals are generally taxed under Capital Gains Tax rules unless the activity is truly a trade. The app models share CGT.

| Item | Rule |
| --- | --- |
| Tax period | UK tax year: 6 April to 5 April. |
| Holding period | No statutory short-term/long-term share CGT split. |
| Matching | Same-day acquisitions, then acquisitions in the following 30 days, then Section 104 pool. |
| Annual exempt amount | `mlai-trade` uses GBP 12,300 through tax year 2022, GBP 6,000 for tax year 2023, and GBP 3,000 from tax year 2024 onward. |
| Rates | `mlai-trade` uses 10%/20% before tax year 2024 and 18%/24% from tax year 2024 onward. |
| Base income | `tax.estimated_annual_income` is used to place gains in the basic-rate or higher-rate band. |

The app uses a simplified basic-rate band threshold of GBP 37,700 for the share CGT calculation. Final HMRC liability can differ because income tax bands, allowances, losses, reliefs, residence, and disposal-date transition rules are fact-specific.

### HMRC Share Identification Order

When shares of the same class in the same company are sold, HMRC matching usually follows:

1. Shares acquired on the same day as the disposal.
2. Shares acquired in the 30 days after the disposal.
3. The pooled holding, commonly called the Section 104 holding.

This is why simple FIFO is not enough for UK share CGT. `mlai-trade` uses provider fill activities when available to build same-day, 30-day, and Section 104 matches.

### Currency Treatment

The UK profile reports amounts in GBP. The app does not perform historical FX conversion. If provider fills are in USD, EUR, BRL, SGD, or another currency, convert and retain evidence before relying on exported totals for filing.

---

## 3. Day Trading Rules

The UK does not use the US Pattern Day Trader framework. Day trading matters because:

- Same-day acquisitions are first in HMRC share matching.
- Frequent activity can raise a fact question about whether activity is investment or a trade.
- Broker, margin, FCA, and exchange rules still apply.

For most individual investors, frequent share disposals still fall under CGT rather than income tax, but this is fact-specific.

---

## 4. Wash Sale Rule

The UK does not have a US Section 1091 wash-sale rule. HMRC's 30-day "bed and breakfasting" rule is a share-identification rule, not a separate loss-disallowance rule and not a legal prohibition on buying.

`mlai-trade` models the 30-day rule as a conservative replacement guard so a user can see when a new acquisition would affect matching for a recent disposal.

---

## 5. Tax-Loss Harvesting

UK loss planning must account for HMRC share matching:

| Item | Treatment |
| --- | --- |
| Same-day buys | Matched before the pool. |
| Buys within 30 days after sale | Matched before the Section 104 pool. |
| Section 104 pool | Tracks pooled quantity and allowable cost. |
| Capital losses | Can offset same-year gains and may carry forward if claimed/reported correctly. |
| Annual exempt amount | Losses and annual exempt amount sequencing can change filing results. |

`mlai-trade` does not import prior-year capital losses or model claims, reliefs, or spouse/civil partner transfers.

---

## 6. Options Trading Tax Rules

UK options, contracts for difference, spread betting, and derivatives can have different tax treatment from ordinary shares.

Common considerations:

- Listed options can produce chargeable gains or income depending on facts and instrument type.
- Exercise, assignment, lapse, and closing transactions can change the timing and amount of gains.
- Spread betting may have separate treatment.
- Employment-related securities and unapproved options have special rules.

`mlai-trade` does not currently model UK option-specific tax rules.

---

## 7. Crypto Trading Tax Rules

Crypto assets are outside the stock-trading model. HMRC generally applies CGT principles to individual crypto disposals, but tokens can also produce income depending on activity, mining, staking, employment, or trading-business facts.

Crypto has its own pooling and identification rules, record requirements, and reporting thresholds. Do not use the UK share profile as a crypto tax engine.

---

## 8. Reporting Requirements

UK tax residents commonly need:

| Requirement | Notes |
| --- | --- |
| Self Assessment | Report chargeable gains where required. |
| Real-time CGT service | May be available for some gains; Self Assessment may still be needed. |
| Records | Keep contract notes, broker statements, FX conversion, and Section 104 pool records. |
| Reporting thresholds | Reporting can be required based on gains, proceeds, or Self Assessment status. |
| Tax shelters | ISA and pension/SIPP accounts need separate treatment and are not modeled. |

`mlai-trade` exports estimates; it does not file HMRC returns.

---

## 9. Trader Tax Status

The UK does not have an elective equivalent to US Section 475 trader tax status for this app. A person's activity may be investment activity taxed under CGT or, in unusual cases, a trade taxed as income. This is determined by facts.

`mlai-trade` models individual share CGT, not trading-business income tax.

---

## 10. Prohibited Activities (ILLEGAL)

Do not use automated or manual trading to:

- Trade on inside information.
- Manipulate prices, liquidity, volume, or benchmark signals.
- Spoof, layer, or submit orders without genuine trading intent.
- Make false statements to brokers, HMRC, FCA, exchanges, or counterparties.
- Circumvent broker, margin, venue, or market access controls.

---

## 11. Algorithmic/Automated Trading Rules

Personal automation should keep enough evidence to explain every order:

- Strategy version and parameters.
- Signal inputs and code version.
- Order submission, cancel, fill, and rejection logs.
- Broker contract notes and account statements.
- Risk controls, margin records, and position limits.
- Section 104 pool and matching evidence.

Broker, venue, and FCA rules may restrict automated, high-frequency, direct-market-access, or market-making strategies. Keep `mlai-trade` logs with tax records.

---

## 12. Foreign Account Considerations

UK tax residents using foreign brokers should separately track:

- GBP conversion for every buy, sell, dividend, fee, and tax withholding.
- Foreign withholding taxes and possible foreign tax credit relief.
- Offshore fund status, reporting fund status, and excess reportable income where relevant.
- Residence, domicile, split-year, and remittance-basis issues where relevant.
- Account statements sufficient to reconstruct Section 104 pools.

The app does not model foreign tax credits, remittance basis, offshore fund rules, or non-dom rules.

---

## 13. Compliance Checklist

- Set `tax.residency_country` to `GB`.
- Confirm reported currency is `GBP`.
- Use provider fill history for accurate same-day/30-day/Section 104 matching.
- Keep FX evidence for non-GBP trades.
- Maintain Section 104 pool records outside the app.
- Track prior-year losses and claims separately.
- Exclude ISA/SIPP accounts from taxable estimates or review them separately.
- Review reporting thresholds and Self Assessment requirements.

---

## 14. Tax Calendar

| Timing | Action |
| --- | --- |
| Every trade date | Save contract notes, fills, fees, and FX evidence. |
| 6 April to 5 April | UK CGT tax year. |
| After tax-year end | Reconcile gains, losses, annual exempt amount, and reporting thresholds. |
| Self Assessment deadline | File and pay by the applicable HMRC deadline when required. |

---

## 15. Recommendations for Our Alpaca Setup

For a UK tax resident:

1. Configure exactly one tax country: `tax.residency_country = "GB"`. `UK` is accepted as an alias but stored as `GB`.
2. Treat all estimates as GBP. If Alpaca activity is USD-denominated, maintain GBP conversion records outside the app.
3. Keep full provider fill history; UK matching needs buys after a sale and a complete Section 104 pool.
4. Use `mlai-trade compliance tax --show-brackets --year YYYY` to confirm the active UK profile and limitations.
5. Use `--details` to review `hmrc_same_day`, `hmrc_30_day`, and `hmrc_section_104` match sources.
