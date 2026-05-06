// Execution-origin classification shared by provider sync, reports, and tax.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOrigin {
    MlaiAuto,
    MlaiCli,
    ProviderExternal,
    Mixed,
    Unknown,
}

impl ExecutionOrigin {
    // Returns the stable JSON/database key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MlaiAuto => "mlai_auto",
            Self::MlaiCli => "mlai_cli",
            Self::ProviderExternal => "provider_external",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }

    // Returns a short human label for tables.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::MlaiAuto => "mlai-auto",
            Self::MlaiCli => "mlai-cli",
            Self::ProviderExternal => "provider",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }

    // Parses a persisted origin key.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "mlai_auto" | "mlai-auto" | "auto" => Self::MlaiAuto,
            "mlai_cli" | "mlai-cli" | "cli" => Self::MlaiCli,
            "provider_external" | "provider" | "external" => Self::ProviderExternal,
            "mixed" => Self::Mixed,
            _ => Self::Unknown,
        }
    }
}

// Classifies known mlai client order id prefixes.
pub fn classify_client_order_id(value: Option<&str>) -> Option<ExecutionOrigin> {
    let value = value?.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    if value.starts_with("mlai-auto-") {
        Some(ExecutionOrigin::MlaiAuto)
    } else if value.starts_with("mlai-cli-") || value.starts_with("plm-") {
        Some(ExecutionOrigin::MlaiCli)
    } else {
        None
    }
}

// Combines entry and exit origins for a realized lot.
pub fn combine(entry: ExecutionOrigin, exit: ExecutionOrigin) -> ExecutionOrigin {
    if entry == ExecutionOrigin::Unknown {
        return exit;
    }
    if exit == ExecutionOrigin::Unknown {
        return entry;
    }
    if entry == exit {
        entry
    } else {
        ExecutionOrigin::Mixed
    }
}

// Creates the local origin override table.
pub fn init_tables(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS order_execution_origins (
            provider TEXT NOT NULL,
            account_ref TEXT NOT NULL,
            paper_account INTEGER NOT NULL,
            order_id TEXT NOT NULL,
            execution_origin TEXT NOT NULL,
            command TEXT,
            recorded_at_utc TEXT NOT NULL,
            PRIMARY KEY(provider, account_ref, paper_account, order_id)
        );
        CREATE INDEX IF NOT EXISTS idx_order_execution_origins_origin
          ON order_execution_origins(execution_origin, recorded_at_utc);",
    )?;
    Ok(())
}

// Records a locally submitted order origin.
pub fn record_order_origin(
    conn: &Connection,
    provider: &str,
    account_ref: &str,
    paper_account: i64,
    order_id: &str,
    origin: ExecutionOrigin,
    command: &str,
) -> anyhow::Result<()> {
    if order_id.trim().is_empty() {
        return Ok(());
    }
    init_tables(conn)?;
    conn.execute(
        "INSERT OR REPLACE INTO order_execution_origins (
            provider, account_ref, paper_account, order_id, execution_origin, command,
            recorded_at_utc
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            provider,
            account_ref,
            paper_account,
            order_id,
            origin.as_str(),
            command,
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
        ],
    )?;
    Ok(())
}

// Classifies an order using client id, local override table, and auto records.
pub fn classify_order(
    conn: &Connection,
    provider: &str,
    account_ref: &str,
    paper_account: i64,
    order_id: Option<&str>,
    client_order_id: Option<&str>,
    known_auto_order: bool,
) -> anyhow::Result<ExecutionOrigin> {
    if let Some(origin) = classify_client_order_id(client_order_id) {
        return Ok(origin);
    }
    if known_auto_order {
        return Ok(ExecutionOrigin::MlaiAuto);
    }
    let Some(order_id) = order_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(ExecutionOrigin::ProviderExternal);
    };
    init_tables(conn)?;
    let override_origin: Option<String> = conn
        .query_row(
            "SELECT execution_origin FROM order_execution_origins
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND order_id=?4",
            params![provider, account_ref, paper_account, order_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(origin) = override_origin {
        return Ok(ExecutionOrigin::parse(&origin));
    }
    let auto_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM auto_trades
             WHERE provider=?1 AND account_ref=?2 AND paper_account=?3 AND order_id=?4",
            params![provider, account_ref, paper_account, order_id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if auto_count > 0 {
        return Ok(ExecutionOrigin::MlaiAuto);
    }
    Ok(ExecutionOrigin::ProviderExternal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_order_id_prefixes_are_classified() {
        assert_eq!(
            classify_client_order_id(Some("mlai-auto-paper-buy-AAPL-1")),
            Some(ExecutionOrigin::MlaiAuto)
        );
        assert_eq!(
            classify_client_order_id(Some("mlai-cli-paper-sell-AAPL-1")),
            Some(ExecutionOrigin::MlaiCli)
        );
        assert_eq!(
            classify_client_order_id(Some("plm-paper-sell-AAPL-1")),
            Some(ExecutionOrigin::MlaiCli)
        );
        assert_eq!(classify_client_order_id(Some("manual-web-id")), None);
    }

    #[test]
    fn mixed_origin_when_entry_and_exit_differ() {
        assert_eq!(
            combine(ExecutionOrigin::MlaiAuto, ExecutionOrigin::MlaiAuto),
            ExecutionOrigin::MlaiAuto
        );
        assert_eq!(
            combine(ExecutionOrigin::MlaiAuto, ExecutionOrigin::ProviderExternal),
            ExecutionOrigin::Mixed
        );
    }
}
