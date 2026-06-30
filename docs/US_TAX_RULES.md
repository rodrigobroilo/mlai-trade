# US (ISO 3166-1 alpha-2: US) Tax & Compliance Rules for Stock Trading — Comprehensive Reference

> **Country Code:** US
> **Reporting Currency:** USD ($)
> **Last Updated:** May 2, 2026
> **Source Verification:** IRS, FINRA, SEC, FinCEN, California FTB pages checked May 2, 2026
> **Purpose:** Complete compliance reference for our Alpaca trading system
> **Status:** Paper trading (no tax events yet) — will apply when going live
> **Disclaimer:** This is a research summary, NOT legal/tax advice. Consult a qualified tax professional before making decisions.

---

## Table of Contents

1. [Executive Summary — Must-Know Rules](#1-executive-summary)
2. [Tax Treatment of Trading Income](#2-tax-treatment-of-trading-income)
3. [Day Trading Rules (PDT)](#3-day-trading-rules-pdt)
4. [Wash Sale Rule (Section 1091)](#4-wash-sale-rule-section-1091)
5. [Tax-Loss Harvesting](#5-tax-loss-harvesting)
6. [Options Trading Tax Rules](#6-options-trading-tax-rules)
7. [Crypto Trading Tax Rules](#7-crypto-trading-tax-rules)
8. [Reporting Requirements](#8-reporting-requirements)
9. [Trader Tax Status (Section 475)](#9-trader-tax-status-section-475)
10. [Prohibited Activities (ILLEGAL)](#10-prohibited-activities--illegal)
11. [Algorithmic/Automated Trading Rules](#11-algorithmicautomated-trading-rules)
12. [Foreign Account Considerations](#12-foreign-account-considerations)
13. [Compliance Checklist](#13-compliance-checklist)
14. [Tax Calendar](#14-tax-calendar)
15. [Recommendations for Our Alpaca Setup](#15-recommendations-for-our-alpaca-setup)

---

## 1. Executive Summary

### 🔑 Rules Every Trader MUST Know

| Rule | Summary | Risk |
|------|---------|------|
| **Short-term vs Long-term gains** | Held ≤1 year = ordinary income (up to 37%). Held >1 year = 0/15/20%. | Higher taxes on frequent trading |
| **Wash Sale Rule** | Can't deduct a loss if you acquire substantially identical securities in the 61-day window: 30 days before sale, sale day, 30 days after | Disallowed losses, basis adjustments, Form 8949 code W |
| **Day Trading Margin Rules** | Current PDT framework is being replaced by FINRA intraday margin standards effective June 4, 2026; brokers can phase implementation through Oct 20, 2027 | Broker-specific restrictions, intraday margin deficits, 90-day freezes |
| **Estimated Tax Payments** | If you owe ≥$1,000 in taxes after withholding, must pay quarterly | Underpayment penalties |
| **Net Investment Income Tax** | Extra 3.8% surtax if MAGI exceeds $200K single/HOH, $250K MFJ/QSS, or $125K MFS | Additional tax on investment income |
| **Capital Loss Limit** | Max $3,000/year ($1,500 MFS) deductible against ordinary income; rest carries forward | Can't offset large losses in one year |
| **California Capital Gains** | California taxes capital gains as ordinary income; no CA preferential long-term capital gain rate | Federal after-tax estimates may be too optimistic |
| **Paper Trading** | NO tax events. Zero reporting. Only matters when you go live. | None while paper trading |

### Source-Verified Updates (May 2, 2026)

These notes override any older wording elsewhere in this file:

1. **PDT is being replaced, but timing is broker-dependent.** FINRA Regulatory Notice 26-10 says amendments to Rule 4210 replace the old day trading margin requirements, including PDT day-count designation and the $25,000 minimum equity requirement. Effective date: **June 4, 2026**. Members may phase implementation through **October 20, 2027**. Until Alpaca confirms account behavior through API/account notices, code should remain conservative.
2. **Wash-sale tracking must be bidirectional.** A loss sale can be disallowed by replacement purchases in the 30 days before the sale as well as purchases in the 30 days after. Our system must track lots and replacement shares, not only block future buys.
3. **Broker reporting is incomplete for full compliance.** Form 1099-B wash-sale reporting may cover only same-account/same-CUSIP covered securities. The taxpayer remains responsible for cross-account, spouse, IRA, and manually corrected Form 8949 reporting.
4. **Trader Tax Status is factual, not elective by label.** IRS Topic 429 requires profit motive from daily market movements, substantial activity, continuity, and regularity. If Section 475(f) mark-to-market is valid and timely, wash-sale rules and capital-loss limits generally stop applying to trading-business securities.
5. **Estimated taxes must be planned when live.** IRS Pub. 505 generally requires estimated payments when expected tax owed is at least $1,000 and withholding/credits are below safe-harbor levels. Trading income can be uneven, so annualized estimated tax calculations may matter.
6. **NIIT may apply.** The 3.8% Net Investment Income Tax can apply above MAGI thresholds of $200,000 single/head of household, $250,000 married filing jointly, and $125,000 married filing separately.
7. **State tax matters.** California taxes all capital gains as ordinary income. Federal long-term capital gains preferences do not apply for California tax.
8. **Algo logs are compliance evidence.** Keep full order, cancel, fill, position, strategy-signal, parameter, and code-version history so every trade can be explained later.

**Primary sources added:** [FINRA Regulatory Notice 26-10](https://www.finra.org/rules-guidance/notices/26-10), [SEC Release 34-105226](https://www.sec.gov/files/rules/sro/finra/2026/34-105226.pdf), [IRS Federal Income Tax Rates and Brackets](https://www.irs.gov/filing/federal-income-tax-rates-and-brackets), [IRS Revenue Procedure 2025-32](https://www.irs.gov/pub/irs-drop/rp-25-32.pdf), [IRS Topic 429](https://www.irs.gov/taxtopics/tc429), [IRS Publication 550](https://www.irs.gov/publications/p550), [IRS Publication 505](https://www.irs.gov/publications/p505), [IRS Form 8949 Instructions](https://www.irs.gov/instructions/i8949), [IRS Form 8960 Instructions](https://www.irs.gov/instructions/i8960), [California FTB Capital Gains](https://www.ftb.ca.gov/file/personal/income-types/capital-gains-and-losses.html)

### ⛔ Things That Are ILLEGAL (See Section 10 for details)
- Insider trading (trading on material non-public information)
- Market manipulation (spoofing, layering, pump & dump)
- Front-running
- Wash trading (creating fake volume)
- Making false statements to brokers/regulators

---

## 2. Tax Treatment of Trading Income

### Short-Term Capital Gains (Held ≤ 1 Year)
Taxed as **ordinary income** at your marginal tax rate.

**2025 Federal Income Tax Brackets (Taxable Income; Returns Filed in 2026):**

| Rate | Single | Married Filing Jointly / Surviving Spouse | Married Filing Separately | Head of Household |
|------|--------|------------------------------------------|---------------------------|-------------------|
| 10% | $0 - $11,925 | $0 - $23,850 | $0 - $11,925 | $0 - $17,000 |
| 12% | $11,926 - $48,475 | $23,851 - $96,950 | $11,926 - $48,475 | $17,001 - $64,850 |
| 22% | $48,476 - $103,350 | $96,951 - $206,700 | $48,476 - $103,350 | $64,851 - $103,350 |
| 24% | $103,351 - $197,300 | $206,701 - $394,600 | $103,351 - $197,300 | $103,351 - $197,300 |
| 32% | $197,301 - $250,525 | $394,601 - $501,050 | $197,301 - $250,525 | $197,301 - $250,500 |
| 35% | $250,526 - $626,350 | $501,051 - $751,600 | $250,526 - $375,800 | $250,501 - $626,350 |
| 37% | $626,351+ | $751,601+ | $375,801+ | $626,351+ |

**2026 Federal Income Tax Brackets (Taxable Income; Returns Filed in 2027):**

| Rate | Single | Married Filing Jointly / Surviving Spouse | Married Filing Separately | Head of Household |
|------|--------|------------------------------------------|---------------------------|-------------------|
| 10% | $0 - $12,400 | $0 - $24,800 | $0 - $12,400 | $0 - $17,700 |
| 12% | $12,401 - $50,400 | $24,801 - $100,800 | $12,401 - $50,400 | $17,701 - $67,450 |
| 22% | $50,401 - $105,700 | $100,801 - $211,400 | $50,401 - $105,700 | $67,451 - $105,700 |
| 24% | $105,701 - $201,775 | $211,401 - $403,550 | $105,701 - $201,775 | $105,701 - $201,750 |
| 32% | $201,776 - $256,225 | $403,551 - $512,450 | $201,776 - $256,225 | $201,751 - $256,200 |
| 35% | $256,226 - $640,600 | $512,451 - $768,700 | $256,226 - $384,350 | $256,201 - $640,600 |
| 37% | $640,601+ | $768,701+ | $384,351+ | $640,601+ |

**Standard Deduction:**

| Tax Year | Single | Married Filing Jointly / Surviving Spouse | Married Filing Separately | Head of Household |
|----------|--------|------------------------------------------|---------------------------|-------------------|
| 2025 | $15,750 | $31,500 | $15,750 | $23,625 |
| 2026 | $16,100 | $32,200 | $16,100 | $24,150 |

> **2027 tax-year note:** As of May 2, 2026, IRS has published tax year 2026 brackets, which apply to returns filed in 2027. IRS has not yet published tax year 2027 brackets. Do not estimate them in code; update this document from IRS guidance when released.

### Long-Term Capital Gains (Held > 1 Year)
Preferential rates:

**2025 Long-Term Capital Gains Rates (Taxable Income):**

| Rate | Single | Married Filing Jointly / Surviving Spouse | Married Filing Separately | Head of Household |
|------|--------|------------------------------------------|---------------------------|-------------------|
| 0% | $0 - $48,350 | $0 - $96,700 | $0 - $48,350 | $0 - $64,750 |
| 15% | $48,351 - $533,400 | $96,701 - $600,050 | $48,351 - $300,000 | $64,751 - $566,700 |
| 20% | $533,401+ | $600,051+ | $300,001+ | $566,701+ |

**2026 Long-Term Capital Gains Rates (Taxable Income; Returns Filed in 2027):**

| Rate | Single | Married Filing Jointly / Surviving Spouse | Married Filing Separately | Head of Household |
|------|--------|------------------------------------------|---------------------------|-------------------|
| 0% | $0 - $49,450 | $0 - $98,900 | $0 - $49,450 | $0 - $66,200 |
| 15% | $49,451 - $545,500 | $98,901 - $613,700 | $49,451 - $306,850 | $66,201 - $579,600 |
| 20% | $545,501+ | $613,701+ | $306,851+ | $579,601+ |

**Special Maximum Long-Term Capital Gains Rates:**
- Collectibles gain: maximum 28%.
- Eligible gain on qualified small business stock minus the Section 1202 exclusion: maximum 28%.
- Unrecaptured Section 1250 gain: maximum 25%.
- Qualified dividends generally use the same 0% / 15% / 20% maximum rate structure as net capital gain.

### Net Investment Income Tax (NIIT)
- Additional **3.8% surtax** on net investment income
- Applies when Modified AGI exceeds:
  - $200,000 (single or head of household)
  - $250,000 (married filing jointly / surviving spouse)
  - $125,000 (married filing separately)
- Investment income includes: capital gains, dividends, interest, rental income, royalties
- **Source:** IRC Section 1411

### Dividend Income
- **Qualified dividends** (held >60 days): taxed at long-term capital gains rates
- **Non-qualified/ordinary dividends**: taxed as ordinary income
- Dividends are taxable even if reinvested (DRIP)

### Paper Trading → Live Trading Transition
- **Paper trading generates ZERO tax events** — no gains, no losses, no reporting
- Taxes only begin when real money changes hands in a live account
- Your paper trading P&L history is irrelevant to the IRS
- When transitioning: your cost basis starts fresh with actual purchase prices

**Sources:** [IRS Federal Income Tax Rates and Brackets](https://www.irs.gov/filing/federal-income-tax-rates-and-brackets), [IRS Revenue Procedure 2025-32](https://www.irs.gov/pub/irs-drop/rp-25-32.pdf), [IRS Publication 550 (2025)](https://www.irs.gov/pub/irs-prior/p550--2025.pdf), [IRS Topic 409](https://www.irs.gov/taxtopics/tc409)

---

## 3. Day Trading Rules (PDT / Intraday Margin)

### Pattern Day Trader Rule — Current Through June 3, 2026

**What is a Day Trade?**
Buying and selling (or selling short and buying to cover) the **same security on the same day** in a margin account.

**What Triggers PDT Status?**
- Executing **4 or more day trades** within **5 business days**
- PROVIDED those day trades represent **more than 6%** of total trades in that period
- Only applies to **margin accounts** (not cash accounts)

### Current Rule (Until June 3, 2026)
- PDT must maintain **minimum equity of $25,000** in margin account at all times
- If equity falls below $25,000 → account restricted, can't day trade
- Broker may issue a **margin call** requiring deposit within 5 business days
- If not met: account restricted to **cash-available trading** for 90 days

### New Intraday Margin Rule (Effective June 4, 2026)
FINRA Regulatory Notice 26-10 announces that Rule 4210 amendments replace the old day trading margin requirements in their entirety:

- The day trade count requirements for designating a customer as a Pattern Day Trader are eliminated.
- The $25,000 PDT minimum equity requirement is eliminated.
- Margin accounts become subject to an **intraday margin level** and **intraday margin deficit** framework based on market exposure during the trading day.
- Brokers must require intraday margin deficits to be satisfied as promptly as possible.
- If a customer makes a practice of failing to satisfy intraday margin deficits and does not satisfy a deficit by the close of the fifth business day, the broker must apply policies to prevent creating or increasing short positions or debit balances for **90 calendar days** or until the deficit is satisfied.
- Members may phase implementation through **October 20, 2027**.

**Implementation note for our system:** Do not remove PDT/intraday safeguards purely by date. Query Alpaca account state and maintain conservative cash-only constraints unless the live account and broker rules clearly support the trade.

### PDT and Account Types

| Account Type | PDT Rule Applies? | Notes |
|-------------|-------------------|-------|
| Margin account before Jun 4, 2026 | Yes | Old PDT framework applies |
| Margin account from Jun 4, 2026 | Replaced | Intraday margin standards apply; broker phase-in may vary |
| Cash account | No | No day trade limit, but T+1 settlement applies |
| Paper trading | No | Not real trades, PDT doesn't apply |

### Cash Account Alternative
- Cash accounts are **exempt** from PDT rules
- **BUT:** subject to T+1 settlement (trades settle next business day)
- Can't sell stock bought with unsettled funds → **free-riding violation** (Regulation T)
- Effectively limits how fast you can rotate capital

### Our Alpaca Setup
- Paper trading account = **PDT does not apply**
- When going live before June 4, 2026: Alpaca margin accounts are subject to PDT rules
- From June 4, 2026: Alpaca says it is implementing the new intraday margin framework, but the bot should still read account/broker state and enforce cash-only mode unless explicitly changed

**Sources:** [FINRA Regulatory Notice 26-10](https://www.finra.org/rules-guidance/notices/26-10), [SEC Release 34-105226](https://www.sec.gov/files/rules/sro/finra/2026/34-105226.pdf), [FINRA Day Trading](https://www.finra.org/investors/investing/investment-products/stocks/day-trading)

---

## 4. Wash Sale Rule (Section 1091)

### Overview
The wash sale rule **disallows a tax deduction** for a loss on a security if you purchase a "substantially identical" security within **30 days before or after** the sale.

### The 61-Day Window
```
30 days BEFORE sale ←── SALE DATE ──→ 30 days AFTER sale
         |_____________ 61-day window _____________|
```
If you buy a substantially identical security ANY time within this 61-day window, the loss is **disallowed**.

### What Happens to Disallowed Losses
- The loss is **NOT permanently lost** — it's **added to the cost basis** of the replacement shares
- This effectively defers (not eliminates) the tax benefit
- The holding period of the original shares carries over to the replacement shares

**Example:**
1. Buy 100 shares of AAPL at $200 ($20,000)
2. Sell 100 shares at $180 ($18,000) → $2,000 loss
3. Within 30 days, buy 100 shares of AAPL at $185 ($18,500)
4. **Result:** $2,000 loss is disallowed. New cost basis = $185 + $20 = $205/share

### What Counts as "Substantially Identical"
The IRS has **never precisely defined** this term, but generally:

| Transaction | Substantially Identical? | Notes |
|------------|------------------------|-------|
| Same stock (e.g., sell AAPL, buy AAPL) | ✅ Yes | Classic wash sale |
| Same stock via different broker | ✅ Yes | Across ALL your accounts |
| Call option on same stock | ⚠️ Likely yes | IRS has ruled options can trigger |
| Put option on same stock | ⚠️ Possibly | Less clear, depends on specifics |
| Convertible bonds of same company | ✅ Yes | If convertible to same stock |
| ETF tracking same index (e.g., sell SPY, buy VOO) | ⚠️ Gray area | IRS hasn't definitively ruled; many advisors say these are NOT identical |
| Different companies in same sector | ❌ No | E.g., sell AAPL, buy MSFT — OK |
| Mutual fund vs ETF tracking same index | ⚠️ Gray area | Conservative position: could be identical |

### Wash Sale Rule and Crypto
- **Current IRS position (as of 2025-2026):** The wash sale rule under Section 1091 technically applies to "stock or securities"
- Crypto is classified as **"property"** by the IRS, NOT as a "security" for wash sale purposes
- **Result:** The wash sale rule **may not currently apply** to crypto-to-crypto transactions
- **⚠️ WARNING:** Multiple legislative proposals have been introduced to extend wash sale rules to digital assets. The Biden administration's FY2025 budget proposed this. Monitor legislation!
- **Conservative approach:** Treat crypto as if wash sales apply

### Wash Sales Across Accounts
- Wash sales apply across **ALL your accounts** (brokerage, IRA, spouse's accounts)
- Buying in an IRA within 30 days of selling at a loss in a taxable account can trigger a wash sale
- **Critical:** The loss may be **permanently disallowed** if the replacement is in an IRA (since you can't adjust cost basis in an IRA)

### Tracking Obligations
- Brokers are required to track wash sales within a **single account** (reported on 1099-B)
- Brokers do **NOT** track wash sales across multiple accounts or across spouses
- **You are responsible** for tracking cross-account wash sales
- Must adjust Form 8949 accordingly

### Implications for Algorithmic Trading
- High-frequency strategies that trade the same stocks repeatedly are **extremely prone** to wash sales
- Our scanner/trading bot must track the full **61-day window** for every loss sale
- The bot must look backward 30 days for replacement purchases already made before allowing a tax-loss sale
- The bot must look forward 30 days after every loss sale and block or flag replacement purchases
- In `mlai-trade`, the IRS 30-day forward replacement window is hardcoded and not configurable. The only configurable value is an additional safety buffer, defaulting to 1 day, so the default replacement-buy block is 31 calendar days after a loss sale.
- Track tax lots, adjusted basis, holding-period carryover, and Form 8949 adjustment code `W`
- Broker 1099-B data is not enough for complete compliance because same-account broker reporting may miss cross-account, spouse, IRA, or non-covered-security wash sales

**Sources:** [26 U.S.C. § 1091](https://www.law.cornell.edu/uscode/text/26/1091), [26 CFR § 1.1091-1](https://www.law.cornell.edu/cfr/text/26/1.1091-1), [IRS Publication 550](https://www.irs.gov/forms-pubs/about-publication-550)

---

## 5. Tax-Loss Harvesting

### What It Is
Strategically selling investments at a loss to **offset capital gains** from other investments, reducing your overall tax bill.

### Rules

1. **Losses offset gains dollar-for-dollar** — short-term losses first offset short-term gains, long-term losses first offset long-term gains
2. **Excess losses** can offset the other type (short-term losses can offset long-term gains and vice versa)
3. **$3,000 cap** on deducting net capital losses against ordinary income per year ($1,500 if married filing separately)
4. **Unlimited carryforward** — excess losses carry forward indefinitely to future years
5. **Must be completed by December 31** — no grace period into the next year
6. **Watch the wash sale rule** — can't buy back substantially identical security within 30 days!

### Legal Strategies

**Strategy 1: Sell and replace with similar (not identical) investment**
- Sell losing position in S&P 500 ETF (e.g., SPY)
- Buy a different S&P 500 ETF (e.g., IVV or VOO) — debatable if "substantially identical"
- Safer: Buy a total market ETF (VTI) which is clearly different
- Wait 31+ days if you want to buy back the original

**Strategy 2: Year-end portfolio cleanup**
- Review portfolio in November/December
- Identify losing positions
- Sell to harvest losses before Dec 31
- Reinvest proceeds in non-identical alternatives

**Strategy 3: Offset concentrated gains**
- If you have large gains from one position, find offsetting losses
- Short-term losses are most valuable (offset income taxed at up to 37%)

### Important Considerations
- Don't let the "tax tail wag the investment dog" — don't sell a good investment just for the tax benefit
- Transaction costs and bid-ask spreads can eat into tax savings
- Must maintain good cost basis records for every lot
- Robo-advisors often do this automatically; our algo should consider it

**Sources:** [NerdWallet Tax-Loss Harvesting Rules](https://www.nerdwallet.com/taxes/learn/tax-loss-harvesting), [IRS Schedule D Instructions](https://www.irs.gov/pub/irs-pdf/i1040sd.pdf)

---

## 6. Options Trading Tax Rules

### Basic Options Taxation

**For the Option Buyer:**

| Event | Tax Treatment |
|-------|--------------|
| Option expires worthless | Capital loss (short-term if held ≤1 year) |
| Option is sold before expiration | Capital gain/loss based on premium paid vs received |
| Call option is exercised | Premium added to cost basis of acquired stock; no tax event until stock is sold |
| Put option is exercised | Premium reduces amount realized on the stock sale |

**For the Option Writer (Seller):**

| Event | Tax Treatment |
|-------|--------------|
| Option expires worthless | Premium received = short-term capital gain |
| Option is bought back (closed) | Gain/loss = premium received minus buyback cost |
| Call assigned (stock called away) | Premium added to sale price of stock |
| Put assigned (must buy stock) | Premium reduces cost basis of acquired stock |

### Section 1256 Contracts (60/40 Rule)

Certain contracts get favorable tax treatment:

**Qualifying contracts:**
- Regulated futures contracts
- Foreign currency contracts
- Non-equity options (options on broad-based indexes like SPX, NDX, RUT)
- Dealer equity options
- Dealer securities futures contracts

**The 60/40 rule:**
- Regardless of how long held: **60% long-term + 40% short-term** capital gain/loss
- Reported on **Form 6781** (not Form 8949)
- **Mark-to-market** at year end — must report unrealized gains/losses as of Dec 31
- Maximum effective tax rate: ~26.8% (vs 37% for pure short-term)

**What does NOT qualify as Section 1256:**
- Individual stock options (e.g., AAPL calls/puts) — these are regular capital assets
- ETF options (e.g., SPY options) — regular capital assets
- Only broad-based INDEX options (SPX, NDX, RUT) qualify

### Straddle Rules (Section 1092)
- A straddle = offsetting positions in the same underlying
- If you hold a straddle, **losses on one leg may be deferred** until the offsetting position is closed
- "Identified straddles" have different rules if properly identified on acquisition date
- Complex — consult a tax professional for straddle positions

### Constructive Sale Rules (Section 1259)
- If you hold an appreciated position and enter into an offsetting position that eliminates virtually all risk:
  - **Treated as if you sold** the appreciated position (constructive sale)
  - Gain is recognized immediately
- Examples that trigger constructive sales:
  - Short sale of identical stock (short against the box)
  - Entering into a futures/forward contract to deliver identical stock
  - Deep in-the-money covered calls (potentially)
- **Purpose:** Prevents locking in gains while deferring tax

**Sources:** [IRC § 1256](https://www.law.cornell.edu/uscode/text/26/1256), [IRC § 1259](https://www.law.cornell.edu/uscode/text/26/1259), [TurboTax Form 6781 Guide](https://turbotax.intuit.com/tax-tips/investments-and-taxes/what-is-form-6781-gains-and-losses-from-section-1256-contracts-and-straddles/L2rfcJXT9)

---

## 7. Crypto Trading Tax Rules

### IRS Classification
- Digital assets (crypto, NFTs, stablecoins) are treated as **property** — NOT currency
- Same capital gains rules apply as for stocks
- **Every** disposal (sell, trade, spend) is a taxable event
- Trading one crypto for another IS a taxable event (unlike stock-for-stock in reorganizations)

### Cost Basis Methods
- **FIFO** (First In, First Out) — default if no method specified
- **LIFO** (Last In, First Out) — may reduce gains in rising market
- **Specific Identification** — choose which lots to sell; requires documentation
- **As of Jan 1, 2025**: Brokers required to use specific identification or FIFO per new regulations
- Must be consistent within an account; method must be designated at time of transfer/sale

### Taxable Events
| Event | Taxable? | Treatment |
|-------|---------|-----------|
| Buy crypto with USD | ❌ No | Establishes cost basis |
| Sell crypto for USD | ✅ Yes | Capital gain/loss |
| Trade crypto for crypto | ✅ Yes | Capital gain/loss on disposed asset |
| Use crypto to buy goods/services | ✅ Yes | Capital gain/loss |
| Receive crypto as payment | ✅ Yes | Ordinary income at FMV |
| Mining income | ✅ Yes | Ordinary income at FMV when received |
| Staking rewards | ✅ Yes | Ordinary income at FMV when received |
| Airdrops | ✅ Yes | Ordinary income at FMV when received |
| Hard fork (if new coins received) | ✅ Yes | Ordinary income at FMV |
| Transfer between own wallets | ❌ No | No taxable event |
| Gifting crypto | ⚠️ Maybe | Gift tax rules apply; recipient inherits cost basis |

### Reporting Requirements
- **Form 8949** — report each sale/disposition
- **Schedule D** — summarize capital gains/losses
- **Form 1040 Digital Asset Question** — "At any time during the tax year, did you receive, sell, send, exchange, or otherwise acquire any digital assets?" Must answer truthfully
- **1099-DA** (new) — brokers will issue starting 2025 for centralized exchanges
- **DeFi reporting** — regulations being phased in; currently self-reported

### Wash Sale Rule for Crypto
- **As of 2025-2026:** Wash sale rule technically does NOT apply to crypto (only to "stock or securities")
- Crypto classified as "property" falls outside Section 1091
- **BUT:** Strong legislative push to close this loophole — could change any time
- **Recommendation:** Track and be prepared; consider treating crypto wash sales as if the rule applies

### Staking and Mining
- Income recognized at **fair market value when received**
- Cost basis established at that FMV
- If later sold, capital gain/loss = sale price minus FMV at receipt
- Self-employment tax may apply to mining income (business activity)
- Staking: IRS position (per Jarrett v. IRS) — taxable as income when received, though still debated

**Sources:** [IRS Digital Asset FAQ](https://www.irs.gov/individuals/international-taxpayers/frequently-asked-questions-on-digital-asset-transactions), [IRS Notice 2014-21](https://www.irs.gov/irb/2014-16_IRB#NOT-2014-21), [Treasury Decision 10000 (2024 Regulations)](https://www.federalregister.gov/documents/2024/07/09/2024-14004/gross-proceeds-and-basis-reporting-by-brokers-and-determination-of-amount-realized-and-basis-for)

---

## 8. Reporting Requirements

### Forms You'll Need

| Form | Purpose | When |
|------|---------|------|
| **1099-B** | Broker reports your proceeds from sales | Received from broker by Feb 15 |
| **1099-DIV** | Dividend income | Received from broker by Feb 15 |
| **1099-INT** | Interest income | Received from broker by Feb 15 |
| **1099-DA** | Digital asset transactions (NEW, starting 2025) | From crypto exchanges |
| **Form 8949** | Report each sale transaction (date, proceeds, basis, gain/loss) | Filed with tax return |
| **Schedule D** | Summary of capital gains and losses | Filed with tax return |
| **Form 6781** | Section 1256 contracts and straddles | Filed with tax return |
| **Form 4797** | Sales of business property (if mark-to-market election) | Filed with tax return |
| **Schedule C** | Business expenses (if trader tax status) | Filed with tax return |
| **Form 1040-ES** | Estimated quarterly tax payments | Quarterly |

### Form 8949 Categories

| Box | Basis Reported to IRS? | Category |
|-----|----------------------|----------|
| A | Yes, short-term | Broker reported basis, held ≤1 year |
| B | No, short-term | No broker-reported basis, held ≤1 year |
| C | N/A, short-term | Form 1099-B not received |
| D | Yes, long-term | Broker reported basis, held >1 year |
| E | No, long-term | No broker-reported basis, held >1 year |
| F | N/A, long-term | Form 1099-B not received |

### Estimated Quarterly Tax Payments

**When Required:**
- If you expect to owe **≥$1,000** in taxes after withholding and credits
- Or if withholding + credits < **lesser of**: 90% of current year tax or 100% of prior year tax (110% if AGI > $150K)

**2026 Due Dates:**
| Quarter | Period | Due Date |
|---------|--------|----------|
| Q1 | Jan 1 – Mar 31 | April 15, 2026 |
| Q2 | Apr 1 – May 31 | June 15, 2026 |
| Q3 | Jun 1 – Aug 31 | September 15, 2026 |
| Q4 | Sep 1 – Dec 31 | January 15, 2027 |

**Penalty:** IRS charges underpayment penalty (currently ~8% annual rate) on late/insufficient estimated payments

### Record-Keeping Requirements
**What to save:**
- All trade confirmations (buy and sell)
- Cost basis records for every lot
- Broker statements (monthly/annual)
- 1099 forms
- Wash sale adjustments
- Any correspondence with broker
- Records of trader tax status election (if applicable)

**How long to keep:**
- **Minimum 3 years** from date of filing (general statute of limitations)
- **6 years** if income underreported by >25%
- **Indefinitely** if fraud suspected or no return filed
- **Practical recommendation: Keep forever** (storage is cheap)

**Sources:** [IRS Form 8949 Instructions](https://www.irs.gov/instructions/i8949), [IRS Publication 505](https://www.irs.gov/publications/p505), [IRS Topic 409](https://www.irs.gov/taxtopics/tc409)

---

## 9. Trader Tax Status (Section 475)

### Investor vs Trader — The Critical Distinction

The IRS distinguishes between **investors** and **traders in securities**:

| Factor | Investor | Trader |
|--------|----------|--------|
| Intent | Profit from dividends, interest, capital appreciation | Profit from daily market movements |
| Activity level | Moderate; buys and holds | Substantial; frequent trading |
| Continuity | Periodic | Regular and continuous |
| Holding period | Longer term | Short term (days to weeks) |
| Tax forms | Schedule D, Form 8949 | Schedule C (expenses), Form 4797 (if MTM) |
| Wash sale rule | Applies | Doesn't apply IF mark-to-market elected |
| Capital loss limit ($3K) | Applies | Doesn't apply IF mark-to-market elected |
| Business expense deductions | Very limited (mostly eliminated by TCJA) | Deductible on Schedule C |

### Qualifying as a Trader
Per IRS Topic 429, you must meet **ALL** of these conditions:
1. Seek to profit from **daily market movements** (not dividends/appreciation)
2. Activity must be **substantial**
3. Must carry on with **continuity and regularity**

**Factors considered:**
- Typical holding periods (shorter = more trader-like)
- Frequency and dollar amount of trades
- Extent pursued as income source
- Time devoted to the activity

> **Warning:** Simply calling yourself a "trader" or "day trader" does not make you one for tax purposes. The IRS looks at actual behavior.

### Mark-to-Market Election (Section 475(f))

**Benefits:**
- All gains/losses treated as **ordinary** (not capital) — reported on Form 4797
- **Wash sale rule does NOT apply**
- **$3,000 capital loss limit does NOT apply** — full losses deductible
- Securities marked to market at year-end (unrealized gains/losses recognized Dec 31)
- No carryforward needed for losses

**Drawbacks:**
- All gains taxed as ordinary income (lose favorable long-term capital gains rates)
- Must recognize unrealized gains at year-end (even if you haven't sold)
- Once elected, hard to revoke (5-year "cooling off" period with non-automatic change procedures)
- **Must qualify as a trader** — investors cannot make this election
- NOT subject to self-employment tax (trading gains/losses from being a trader)

### How to Make the Election
1. **Timing:** Must elect by the **due date (without extensions)** of the tax return for the year **BEFORE** the election takes effect
   - Example: To elect for 2026, must file statement with/before April 15, 2026 (your 2025 tax return due date)
2. **New taxpayers:** Within 2 months and 15 days of the start of the tax year
3. **Statement must include:**
   - That you're making an election under Section 475(f)
   - The first tax year the election is effective
   - The trade or business for which you're making the election
4. **Attach to tax return** (or extension request)
5. **File Form 3115** (Application for Change in Accounting Method) under Rev. Proc. 2025-23, Section 24.01

### Business Expense Deductions (Schedule C)
If you qualify as a trader, you can deduct:
- Home office expenses
- Computer and software costs
- Market data subscriptions
- Education/training
- Internet and phone (business portion)
- Professional fees (accountant, tax advisor)
- Trading platform fees
- **Note:** NOT subject to 2% AGI floor (that was for investors, now eliminated by TCJA anyway)

**Sources:** [IRS Topic 429](https://www.irs.gov/taxtopics/tc429), [IRC § 475(f)](https://www.law.cornell.edu/uscode/text/26/475), [Rev. Proc. 2025-23](https://www.irs.gov/pub/irs-drop/rp-25-23.pdf)

---

## 10. Prohibited Activities — ⛔ ILLEGAL

### ⛔ INSIDER TRADING

**What it is:** Trading securities based on **material, non-public information (MNPI)**

**Elements:**
- **Material:** Information that a reasonable investor would consider important in making an investment decision
- **Non-public:** Not yet disseminated to the general public
- Includes: earnings before announcement, merger/acquisition plans, FDA approvals, executive departures

**Who can be liable:**
- Corporate insiders (officers, directors, employees)
- Anyone who receives MNPI from an insider (tippees)
- Anyone who misappropriates MNPI (e.g., from employer, spouse, friend)
- **You don't have to be an "insider"** — trading on a hot tip from a friend who works at the company counts

**Penalties:**
- **Civil:** Up to **3x the profits gained or losses avoided** (treble damages)
- **Criminal:** Up to **$5 million fine** and **20 years in prison** per violation
- SEC can also bar you from serving as officer/director of any public company
- **Real examples:** SEC charged 583 defendants in insider trading cases from 2009-2020

> ⛔ **FOR US:** Never trade based on non-public information from any source. If you have access to material information about a company (through work, friends, or any other channel), do NOT trade that company's stock.

---

### ⛔ MARKET MANIPULATION

**Spoofing (Section 747 of Dodd-Frank Act / 7 U.S.C. § 6c(a)(5))**
- Placing orders with **intent to cancel** before execution
- Purpose: create false impression of supply/demand to move prices
- **Penalty:** Up to **$1 million fine** and **10 years in prison** per offense
- Even algorithmic/automated spoofing is illegal
- **Famous case:** Navinder Sarao sentenced for spoofing that contributed to 2010 Flash Crash

**Layering**
- Placing multiple non-bona fide orders at different price levels to create artificial depth
- A form of spoofing
- SEC and FINRA actively monitor for this using market surveillance
- **Our algos MUST NOT place orders intended to be canceled**

**Pump and Dump**
- Artificially inflating a stock's price through false/misleading statements
- Then selling at the inflated price
- **Penalty:** Up to **$5 million fine** and **20 years in prison** (securities fraud)
- Common in penny stocks and crypto
- **Even promoting a stock you own without disclosing your position can be illegal**

**Wash Trading**
- Buying and selling the same security to create appearance of trading activity
- Creates false impression of market interest/volume
- Illegal under Section 9(a)(1) of the Securities Exchange Act
- **Our algos must NOT create wash trades** (rapidly buying and selling same security with no real purpose other than volume)

**Front-Running**
- Trading ahead of a known pending order (usually by a broker/dealer)
- If you work for a broker and trade ahead of client orders = federal crime
- For retail traders: generally not applicable unless you have access to order flow data

**Painting the Tape**
- Executing trades to create the impression of active trading to attract other investors
- A form of market manipulation

---

### ⛔ MAKING FALSE STATEMENTS
- Providing false information on brokerage applications
- Misrepresenting identity or financial status
- **Penalty:** Up to **$10,000 fine** and **5 years in prison** (18 U.S.C. § 1001)

---

### How This Applies to Algorithmic Trading

⛔ **Our algorithms MUST NOT:**
1. Place orders intended to be canceled (spoofing)
2. Create artificial volume (wash trading)
3. Act on any non-public information
4. Manipulate prices through coordinated trading
5. Spread false information about securities

✅ **Our algorithms CAN:**
1. Execute legitimate buy/sell strategies based on public market data
2. Use technical indicators (RSI, MACD, moving averages, etc.)
3. Respond to public news and events
4. Set limit orders, stop losses, and other standard order types
5. Cancel orders for legitimate reasons (market conditions changed, risk management)

**Sources:** [Securities Exchange Act § 9(a)](https://www.law.cornell.edu/uscode/text/15/78i), [SEC Insider Trading Cases](https://www.sec.gov/spotlight/insidertrading/cases.shtml), [Dodd-Frank Act § 747](https://www.law.cornell.edu/uscode/text/7/6c), [FINRA Manipulative Trading Guide](https://www.finra.org/rules-guidance/guidance/reports/2024-finra-annual-regulatory-oversight-report/manipulative-trading)

---

## 11. Algorithmic/Automated Trading Rules

### Retail Algo Trading — What Applies

As a **retail algorithmic trader** using Alpaca's API, you are subject to:

1. **All standard SEC/FINRA rules** — same as manual trading
2. **Regulation SHO** — short selling rules (locate requirement, uptick rule for restricted stocks)
3. **Regulation T** — margin/credit requirements (T+1 settlement)
4. **Market manipulation rules** — algorithms are NOT exempt; intent is judged by outcome/pattern
5. **Best execution** — Alpaca (as broker) has this obligation, not you directly, but your orders go through their system

### Record-Keeping for Algo Trades
- Keep **all source code** and version history of your trading algorithms
- Log **every order** (placed, modified, canceled, filled) with timestamps
- Document **strategy logic** and any changes made
- Maintain **parameter history** (what thresholds, signals, etc.)
- In case of regulatory inquiry, you must be able to explain why each trade was made
- **Recommendation:** Our scanner DB and trade logs serve this purpose well

### SEC Market Access Rule (Rule 15c3-5)
- Applies primarily to **broker-dealers**, not retail
- Alpaca must have pre-trade risk controls
- As retail API users, Alpaca's systems enforce position limits and buying power checks

### FINRA Rules for Electronic Trading
- **Rule 3110** — Supervision requirements (applies to broker-dealers)
- **Rule 5210** — Publication of transactions (anti-manipulation)
- **Rule 6140** — Other trading practices (short sales, etc.)
- Most of these are Alpaca's responsibility as the broker, but your algos must not cause violations

### Practical Compliance for Our System
1. **Order rate limits:** Alpaca has API rate limits; respect them
2. **Position limits:** Don't exceed buying power
3. **Cancel rates:** High cancel-to-fill ratios may trigger broker surveillance
4. **Consistent strategy:** Be able to explain your strategy if asked
5. **Error handling:** Have kill switches and position limits in your code
6. **Audit trail:** Log everything

---

## 12. Foreign Account Considerations

### FBAR (FinCEN Form 114)

**Who must file:**
- US persons, as defined by FinCEN/IRS rules, who have a financial interest in or signature authority over foreign financial accounts
- When the **aggregate value** of all foreign accounts exceeds **$10,000** at any time during the year

**Trading-system relevance:**
- If a taxpayer has bank accounts, investment accounts, or other financial accounts outside the United States
- And the aggregate value exceeds $10,000 at any point during the year
- FBAR filing may be required

**Filing details:**
- Filed electronically through BSA E-Filing System
- **Due date:** April 15 (automatic extension to October 15)
- **Penalty for non-filing:** Up to **$10,000 per violation** (non-willful); up to **$100,000 or 50% of account balance** (willful)
- **Criminal penalty:** Up to $250,000 and 5 years in prison (willful)

### FATCA (Foreign Account Tax Compliance Act)

**Form 8938 — Statement of Specified Foreign Financial Assets**

**Example filing thresholds for a specified individual living in the US:**
- End of year: total value > **$50,000**
- Any time during year: total value > **$75,000**

**What to report:**
- Foreign bank accounts
- Foreign brokerage accounts
- Foreign mutual funds
- Foreign hedge funds
- Foreign stocks/securities held outside US broker

**Key differences from FBAR:**
| | FBAR | FATCA (Form 8938) |
|---|------|------------------|
| Threshold | $10,000 | $50,000 (end of year) |
| Filed with | FinCEN (separate) | IRS (with tax return) |
| Due date | April 15 (auto ext Oct 15) | With tax return |
| Covers | Bank + financial accounts | Broader: any foreign financial asset |
| Penalty | $10,000+ per violation | $10,000 for failure; up to $50,000 for continued failure |

### Cross-Border Tax Considerations

For taxpayers with foreign financial accounts or foreign broker activity:
1. US tax reporting may include worldwide income, depending on the taxpayer's filing facts.
2. Foreign tax credits may be available for foreign taxes paid (Form 1116).
3. Foreign financial accounts may require FBAR and/or FATCA reporting if thresholds are met.
4. Trading through Alpaca as a US broker generally does not create foreign-account reporting for the Alpaca account itself.
5. Trading through foreign brokers simultaneously can create additional reporting obligations.
6. Tax treaties may affect taxation and reporting for specific countries.
7. Long-term residency or expatriation facts can trigger specialized rules, including Section 877A exit-tax analysis.

### Practical Impact on Our Trading Setup
- **Alpaca is a US broker** → no FBAR/FATCA implications for Alpaca account
- Only relevant if the taxpayer has foreign financial accounts elsewhere
- Alpaca will issue 1099-B with all trade data for US tax reporting

**Sources:** [FinCEN FBAR Requirements](https://www.fincen.gov/report-foreign-bank-and-financial-accounts), [IRS FATCA Information](https://www.irs.gov/businesses/corporations/foreign-account-tax-compliance-act-fatca), [IRS Form 8938](https://www.irs.gov/forms-pubs/about-form-8938)

---

## 13. Compliance Checklist

### Before Going Live
- [ ] Understand capital gains tax implications at your income level
- [ ] Decide on account type: cash vs margin (impacts PDT before Jun 4, 2026 and intraday margin after Jun 4, 2026)
- [ ] Set up full lot-level wash-sale tracking in our trading system (30 days before and after each loss sale)
- [ ] Track settled cash / good-faith / free-riding risk for cash accounts
- [ ] Track broker account restrictions, day-trading state, and intraday margin behavior from Alpaca API/account notices
- [ ] Determine if you'll pursue Trader Tax Status (consult CPA)
- [ ] If electing MTM: file by April 15 of the year BEFORE it takes effect
- [ ] Set up estimated quarterly tax payment reminders
- [ ] Ensure algorithm doesn't create spoofing/manipulation patterns
- [ ] Build order, cancel, fill, position, strategy-signal, parameter, and code-version logging into the system
- [ ] Review Alpaca's margin and trading rules

### Ongoing (When Live)
- [ ] Track all trades with full cost basis
- [ ] Monitor wash sales (full 61-day window on every loss)
- [ ] Reconcile broker 1099-B data against internal lot records
- [ ] Pay estimated quarterly taxes if required
- [ ] Review positions before year-end for tax-loss harvesting opportunities
- [ ] Keep all trade logs, algorithm source code, and strategy documentation
- [ ] Verify 1099-B accuracy when received from Alpaca
- [ ] File FBAR if foreign account balances exceed $10,000

### Annual Tax Filing
- [ ] Collect 1099-B, 1099-DIV, 1099-INT from Alpaca
- [ ] Complete Form 8949 for all sales
- [ ] Complete Schedule D for capital gains summary
- [ ] File Form 6781 if any Section 1256 contracts
- [ ] File Schedule C if claiming Trader Tax Status
- [ ] File Form 4797 if mark-to-market elected
- [ ] Check for foreign account reporting (FBAR/FATCA)
- [ ] Consider tax-loss harvesting before Dec 31

---

## 14. Tax Calendar

| Date | Action | Notes |
|------|--------|-------|
| **January 15** | Q4 estimated tax payment due | For prior year Sep-Dec income |
| **January 31** | Brokers mail 1099-B forms | May arrive as late as Feb 15 |
| **March 15** | S-Corp/Partnership returns due | If applicable |
| **April 15** | Tax return due (or extension) | File or extend! |
| **April 15** | Q1 estimated tax payment due | Jan-Mar income |
| **April 15** | MTM election deadline | For NEXT year's election |
| **April 15** | FBAR due (auto ext to Oct 15) | Foreign accounts >$10K |
| **June 15** | Q2 estimated tax payment due | Apr-May income |
| **September 15** | Q3 estimated tax payment due | Jun-Aug income |
| **October 15** | Extended tax return due | If extension filed |
| **October 15** | FBAR extended deadline | |
| **November-December** | Tax-loss harvesting window | Review portfolio for losses |
| **December 31** | Last day for tax-loss harvesting | Must sell by market close |
| **December 31** | Mark-to-market recognition | Unrealized gains/losses recognized (if MTM elected) |

---

## 15. Recommendations for Our Alpaca Setup

### Current State: Paper Trading
- **No tax implications whatsoever** — trade freely, test strategies
- Use this time to build proper tracking and logging
- Document everything for when we go live

### When Going Live — Recommended Actions

1. **Build Wash Sale Tracker**
   - Track every sell at a loss
   - Flag if same/similar security was bought during the 30 days before the sale
   - Block or flag same/similar security buys during the 30 days after the sale
   - Adjust cost basis automatically and preserve holding-period carryover
   - Store Form 8949 adjustment code `W` and adjustment amount
   - This is the #1 compliance risk for algorithmic traders

2. **Implement Trade Logging**
   - Log every order: timestamp, symbol, side, quantity, price, fill price
   - Log cancels, replaces, rejected orders, and broker error payloads
   - Include strategy/signal that triggered the trade
   - Include code version and model/parameter version
   - Store in database with full audit trail
   - Export capability for tax filing (Form 8949/Schedule D fields)

3. **Consider Account Type Carefully**
   - **Cash account:** No PDT restrictions, simpler, but T+1 settlement limits
   - **Margin account before June 4, 2026:** Old PDT rules apply if day trading
   - **Margin account from June 4, 2026:** New intraday margin standards replace PDT, but broker implementation may vary through Oct 20, 2027
   - Our paper account simulates margin — real account should match strategy needs

4. **Tax Strategy Decision**
   - **If trading frequently (daily):** Consider Trader Tax Status + MTM election
     - Eliminates wash sale headache
     - Full loss deduction
     - But: all gains taxed as ordinary income (lose LTCG rates)
   - **If trading less frequently:** Standard investor treatment may be better
     - Keep some positions >1 year for LTCG rates
     - Must track wash sales

5. **Estimated Tax Payments**
   - Set up quarterly payment reminders when live
   - Calculate estimated tax liability each quarter
   - Pay on time to avoid penalties

6. **Consult a Tax Professional**
   - Before going live, get a CPA who specializes in trader taxes
   - Key decisions: Trader Tax Status, MTM election, entity structure
   - Cost is deductible as a business expense if qualified as trader

7. **Algorithm Compliance**
   - Add order rate limiting
   - Implement kill switches
   - Monitor cancel-to-fill ratios
   - Never code strategies that could be construed as manipulation
   - Log strategy intent for each trade

---

## Appendix: Key IRS Publications & References

| Resource | Description |
|----------|-------------|
| [Publication 550](https://www.irs.gov/forms-pubs/about-publication-550) | Investment Income and Expenses |
| [Publication 544](https://www.irs.gov/forms-pubs/about-publication-544) | Sales and Dispositions of Assets |
| [Publication 505](https://www.irs.gov/publications/p505) | Tax Withholding and Estimated Tax |
| [Topic 409](https://www.irs.gov/taxtopics/tc409) | Capital Gains and Losses |
| [Topic 429](https://www.irs.gov/taxtopics/tc429) | Traders in Securities |
| [IRC § 1091](https://www.law.cornell.edu/uscode/text/26/1091) | Wash Sale Rule |
| [IRC § 475](https://www.law.cornell.edu/uscode/text/26/475) | Mark-to-Market Accounting |
| [IRC § 1256](https://www.law.cornell.edu/uscode/text/26/1256) | Section 1256 Contracts |
| [IRC § 1259](https://www.law.cornell.edu/uscode/text/26/1259) | Constructive Sales |
| [FINRA Rule 4210](https://www.finra.org/rules-guidance/rulebooks/finra-rules/4210) | Margin Requirements |
| [Regulation T](https://www.ecfr.gov/current/title-12/chapter-II/subchapter-A/part-220) | Credit by Brokers |
| [Form 8949 Instructions](https://www.irs.gov/instructions/i8949) | Sales and Dispositions Reporting |
| [IRS Digital Asset FAQ](https://www.irs.gov/individuals/international-taxpayers/frequently-asked-questions-on-digital-asset-transactions) | Crypto Tax Rules |

---

*This document is a research compilation from public IRS, SEC, FINRA, FinCEN, and California FTB sources. It is not legal or tax advice. Tax law is complex and changes frequently. Consult a qualified tax professional before making tax-related decisions. Last researched: May 2, 2026.*
