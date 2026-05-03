// Shared compliance constants and guardrails.
//
// Regulatory/statutory floors live here as code constants. Config may add
// safety buffers, but it must not reduce these floors.
//
// Function map:
// - wash_sale_safety_buffer_days(): clamps user buffers to the legal floor.
// - wash_sale_forward_block_days(): returns the enforced replacement window.

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

pub fn wash_sale_safety_buffer_days(configured: Option<i64>) -> i64 {
    configured
        .unwrap_or(DEFAULT_WASH_SALE_SAFETY_BUFFER_DAYS)
        .max(MIN_WASH_SALE_SAFETY_BUFFER_DAYS)
}

pub fn wash_sale_forward_block_days(configured_buffer: Option<i64>) -> i64 {
    IRS_WASH_SALE_WINDOW_DAYS + wash_sale_safety_buffer_days(configured_buffer)
}
