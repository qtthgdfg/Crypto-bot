//! # bot_exchange
//!
//! Defines the `Exchange` trait — the single abstraction over any
//! supported trading venue.  The rest of the codebase only ever
//! imports this trait; swapping Binance for Bybit/OKX requires
//! adding one new `impl Exchange` without touching anything else.

pub mod binance;
pub mod paper;

use async_trait::async_trait;
use bot_types::{Candle, Interval, OrderRequest, OrderResult, Price, Symbol};
use rust_decimal::Decimal;
use std::collections::HashMap;
use thiserror::Error;

// ── Error ─────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ExchangeError {
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Exchange API error {code}: {message}")]
    Api { code: i64, message: String },

    #[error("Response parse error: {0}")]
    Parse(String),

    #[error("Rate limit exceeded — back off and retry")]
    RateLimit,

    #[error("Insufficient balance for order")]
    InsufficientBalance,

    #[error("Symbol not found: {0}")]
    SymbolNotFound(String),

    #[error("Order rejected: {0}")]
    OrderRejected(String),

    #[error("Paper mode: operation not available")]
    PaperOnly,
}

// ── Ticker snapshot ───────────────────────────────────────────────

/// 24-hour rolling statistics for one symbol.
#[derive(Debug, Clone)]
pub struct Ticker24h {
    pub symbol:       Symbol,
    pub price_change_pct: f64,
    pub last_price:   Price,
    pub volume:       Decimal, // base
    pub quote_volume: Decimal, // quote (USDT)
    pub high:         Price,
    pub low:          Price,
    pub count:        u64,     // number of trades
}

// ── Exchange trait ────────────────────────────────────────────────

/// Async, object-safe interface to a trading venue.
///
/// Implementations must be `Send + Sync` so they can live inside
/// `Arc<dyn Exchange>` and be shared across Tokio tasks.
#[async_trait]
pub trait Exchange: Send + Sync {
    /// Human-readable venue name, e.g. `"Binance"` or `"Paper"`.
    fn name(&self) -> &'static str;

    /// Fetch the current mid-price for one symbol.
    async fn price(&self, symbol: &Symbol) -> Result<Price, ExchangeError>;

    /// Batch-fetch current prices.  Implementors should use a
    /// single API call where the exchange supports it.
    async fn prices(
        &self,
        symbols: &[Symbol],
    ) -> Result<HashMap<Symbol, Price>, ExchangeError>;

    /// Fetch historical OHLCV candles.
    async fn klines(
        &self,
        symbol:   &Symbol,
        interval: Interval,
        limit:    u32,
    ) -> Result<Vec<Candle>, ExchangeError>;

    /// Submit a market order and return the fill result.
    async fn place_order(
        &self,
        req: OrderRequest,
    ) -> Result<OrderResult, ExchangeError>;

    /// Cancel every open order on `symbol`.
    async fn cancel_all(&self, symbol: &Symbol) -> Result<(), ExchangeError>;

    /// Available balance for an asset (e.g. `"USDT"`).
    async fn balance(&self, asset: &str) -> Result<Decimal, ExchangeError>;

    /// Return all actively trading USDT-quoted spot symbols.
    async fn usdt_symbols(&self) -> Result<Vec<Symbol>, ExchangeError>;

    /// Return 24-h rolling statistics for all USDT symbols.
    async fn tickers_24h(&self) -> Result<Vec<Ticker24h>, ExchangeError>;
}
