//! # bot_db
//!
//! Persistence layer. Defines a `Store` trait so the rest of the
//! codebase never imports SQLite directly — swapping the backend
//! (Postgres, Sled, …) only requires a new impl of `Store`.

use async_trait::async_trait;
use bot_types::{Metrics, Outcome, Signal, TradeRecord};
use chrono::Utc;
use rust_decimal::prelude::*;
use rusqlite::{params, Connection};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, instrument};
use uuid::Uuid;

// ── Error ─────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Serialisation error: {0}")]
    Serde(String),
}

// ── Store trait ───────────────────────────────────────────────────

/// All persistence operations the bot needs.
///
/// The trait is `async` (via `async_trait`) and object-safe so it
/// can be stored as `Arc<dyn Store>`.
#[async_trait]
pub trait Store: Send + Sync {
    async fn save_signal(&self, s: &Signal) -> Result<(), DbError>;
    async fn close_trade(&self, sig_id: Uuid, outcome: &Outcome,
                         exit: f64, pnl: f64) -> Result<(), DbError>;
    async fn capital(&self) -> f64;
    async fn daily_pnl(&self) -> f64;
    async fn reset_daily(&self) -> Result<(), DbError>;
    async fn metrics(&self) -> Metrics;
    async fn setup_stats(&self) -> Vec<SetupStat>;
    async fn symbol_stats(&self) -> Vec<SymbolStat>;
}

// ── Per-setup and per-symbol stats (used by Advisor) ─────────────

#[derive(Debug, Default, Clone)]
pub struct SetupStat {
    pub kind:      String,
    pub total:     usize,
    pub wins:      usize,
    pub total_pnl: f64,
}

impl SetupStat {
    pub fn win_rate(&self) -> f64 {
        if self.total == 0 { 0.0 } else { self.wins as f64 / self.total as f64 }
    }
}

#[derive(Debug, Default, Clone)]
pub struct SymbolStat {
    pub symbol:    String,
    pub total:     usize,
    pub wins:      usize,
    pub total_pnl: f64,
}

impl SymbolStat {
    pub fn win_rate(&self) -> f64 {
        if self.total == 0 { 0.0 } else { self.wins as f64 / self.total as f64 }
    }
}

// ── SQLite implementation ─────────────────────────────────────────

/// Thread-safe SQLite-backed store.
///
/// All access is serialised through a `Mutex<Connection>`.  
/// For high-throughput write paths, consider WAL mode + a
/// connection pool (e.g., `r2d2-sqlite`).
pub struct SqliteStore {
    conn:            Mutex<Connection>,
    starting_capital: f64,
}

impl SqliteStore {
    /// Open (or create) the database at `path`, run migrations,
    /// and insert the bootstrap account row.
    pub async fn connect(path: &str) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        // Enable WAL for better concurrent read performance
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let store = Self { conn: Mutex::new(conn), starting_capital: 1.0 };
        store.migrate().await?;
        Ok(store)
    }

    /// Idempotent schema migrations.
    async fn migrate(&self) -> Result<(), DbError> {
        let conn = self.conn.lock().await;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY
             );

             CREATE TABLE IF NOT EXISTS signals (
                id         TEXT PRIMARY KEY,
                symbol     TEXT    NOT NULL,
                direction  TEXT    NOT NULL,
                kind       TEXT    NOT NULL,
                entry      REAL    NOT NULL,
                tp         REAL    NOT NULL,
                sl         REAL    NOT NULL,
                rr         REAL    NOT NULL,
                confidence REAL    NOT NULL,
                size_usd   REAL    NOT NULL,
                reasons    TEXT    NOT NULL,
                created_at INTEGER NOT NULL
             );

             CREATE TABLE IF NOT EXISTS trades (
                id         TEXT    PRIMARY KEY,
                sig_id     TEXT    NOT NULL REFERENCES signals(id),
                outcome    TEXT    NOT NULL,
                exit_price REAL,
                pnl_usd    REAL,
                closed_at  INTEGER,
                CHECK(outcome IN ('WIN','LOSS','OPEN'))
             );

             CREATE INDEX IF NOT EXISTS idx_trades_sig  ON trades(sig_id);
             CREATE INDEX IF NOT EXISTS idx_trades_out  ON trades(outcome);
             CREATE INDEX IF NOT EXISTS idx_signals_sym ON signals(symbol);

             CREATE TABLE IF NOT EXISTS account (
                id          INTEGER PRIMARY KEY CHECK(id = 1),
                capital     REAL    NOT NULL DEFAULT 1.0,
                peak        REAL    NOT NULL DEFAULT 1.0,
                total_pnl   REAL    NOT NULL DEFAULT 0.0,
                daily_pnl   REAL    NOT NULL DEFAULT 0.0,
                last_reset  INTEGER NOT NULL DEFAULT 0
             );

             INSERT OR IGNORE INTO account(id,capital,peak,total_pnl,daily_pnl,last_reset)
             VALUES(1, 1.0, 1.0, 0.0, 0.0, 0);",
        )?;
        Ok(())
    }
}

#[async_trait]
impl Store for SqliteStore {
    #[instrument(skip(self, s))]
    async fn save_signal(&self, s: &Signal) -> Result<(), DbError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO signals
             (id,symbol,direction,kind,entry,tp,sl,rr,confidence,size_usd,reasons,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                s.id.to_string(),
                s.symbol.as_str(),
                s.direction.to_string(),
                s.kind.to_string(),
                s.entry.to_f64(),
                s.take_profit.to_f64(),
                s.stop_loss.to_f64(),
                s.risk_reward,
                s.confidence,
                s.size_usd.to_f64().unwrap_or(0.0),
                s.reasons.join("|"),
                s.created_at.timestamp(),
            ],
        )?;
        debug!(signal_id = %s.id, symbol = %s.symbol, "Signal persisted");
        Ok(())
    }

    #[instrument(skip(self))]
    async fn close_trade(
        &self, sig_id: Uuid, outcome: &Outcome,
        exit: f64, pnl: f64,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock().await;
        let trade_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT OR REPLACE INTO trades(id,sig_id,outcome,exit_price,pnl_usd,closed_at)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                trade_id,
                sig_id.to_string(),
                outcome.to_string(),
                exit,
                pnl,
                Utc::now().timestamp(),
            ],
        )?;
        // Update account balance and peak
        conn.execute(
            "UPDATE account SET
               capital   = capital   + ?1,
               total_pnl = total_pnl + ?1,
               daily_pnl = daily_pnl + ?1,
               peak      = MAX(peak, capital + ?1)
             WHERE id = 1",
            params![pnl],
        )?;
        debug!(pnl, %outcome, "Trade closed");
        Ok(())
    }

    async fn capital(&self) -> f64 {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT capital FROM account WHERE id=1", [], |r| r.get(0))
            .unwrap_or(self.starting_capital)
    }

    async fn daily_pnl(&self) -> f64 {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT daily_pnl FROM account WHERE id=1", [], |r| r.get(0))
            .unwrap_or(0.0)
    }

    async fn reset_daily(&self) -> Result<(), DbError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE account SET daily_pnl=0.0, last_reset=?1 WHERE id=1",
            params![Utc::now().timestamp()],
        )?;
        Ok(())
    }

    async fn metrics(&self) -> Metrics {
        let conn = self.conn.lock().await;
        let capital: f64 = conn
            .query_row("SELECT capital FROM account WHERE id=1", [], |r| r.get(0))
            .unwrap_or(self.starting_capital);

        let mut wins = 0usize; let mut losses = 0usize;
        let mut total_win = 0.0f64; let mut total_loss = 0.0f64;
        let mut equity = vec![self.starting_capital];
        let mut running = self.starting_capital;

        if let Ok(mut stmt) = conn.prepare(
            "SELECT outcome, pnl_usd FROM trades ORDER BY closed_at ASC"
        ) {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok((r.get::<_,String>(0)?, r.get::<_,f64>(1)?))
            }) {
                for (outcome, pnl) in rows.flatten() {
                    running += pnl;
                    equity.push(running);
                    match outcome.as_str() {
                        "WIN"  => { wins   += 1; total_win  += pnl;       }
                        "LOSS" => { losses += 1; total_loss += pnl.abs(); }
                        _ => {}
                    }
                }
            }
        }

        let total = wins + losses;
        let win_rate = if total > 0 { wins as f64 / total as f64 } else { 0.0 };
        let avg_win  = if wins   > 0 { total_win  / wins   as f64 } else { 0.0 };
        let avg_loss = if losses > 0 { total_loss / losses as f64 } else { 1.0 };
        let profit_factor = if total_loss > 0.0 { total_win / total_loss } else { total_win };

        // Max drawdown from equity curve
        let mut max_dd = 0.0f64;
        let mut peak = self.starting_capital;
        for &e in &equity {
            if e > peak { peak = e; }
            let dd = if peak > 0.0 { (peak - e) / peak } else { 0.0 };
            if dd > max_dd { max_dd = dd; }
        }

        // Quarter-Kelly
        let b     = avg_win / avg_loss.max(1e-9);
        let p     = win_rate;
        let raw_k = (p * b - (1.0 - p)) / b.max(1e-9);
        let kelly = (raw_k * 0.25_f64).max(0.01).min(0.10);

        Metrics {
            total, wins, losses, win_rate,
            avg_win_usd: avg_win,
            avg_loss_usd: avg_loss,
            profit_factor,
            total_pnl: total_win - total_loss,
            max_drawdown: max_dd,
            kelly_fraction: kelly,
            capital,
        }
    }

    async fn setup_stats(&self) -> Vec<SetupStat> {
        let conn = self.conn.lock().await;
        let mut map: std::collections::HashMap<String, SetupStat> = Default::default();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT s.kind, t.outcome, t.pnl_usd
             FROM signals s JOIN trades t ON s.id = t.sig_id"
        ) {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?, r.get::<_,f64>(2)?))
            }) {
                for (kind, outcome, pnl) in rows.flatten() {
                    let e = map.entry(kind.clone()).or_insert(SetupStat {
                        kind: kind.clone(), ..Default::default()
                    });
                    e.total += 1;
                    if outcome == "WIN" { e.wins += 1; }
                    e.total_pnl += pnl;
                }
            }
        }
        map.into_values().collect()
    }

    async fn symbol_stats(&self) -> Vec<SymbolStat> {
        let conn = self.conn.lock().await;
        let mut map: std::collections::HashMap<String, SymbolStat> = Default::default();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT s.symbol, t.outcome, t.pnl_usd
             FROM signals s JOIN trades t ON s.id = t.sig_id"
        ) {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?, r.get::<_,f64>(2)?))
            }) {
                for (sym, outcome, pnl) in rows.flatten() {
                    let e = map.entry(sym.clone()).or_insert(SymbolStat {
                        symbol: sym.clone(), ..Default::default()
                    });
                    e.total += 1;
                    if outcome == "WIN" { e.wins += 1; }
                    e.total_pnl += pnl;
                }
            }
        }
        map.into_values().collect()
    }
}
