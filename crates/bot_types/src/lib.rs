//! # bot_types
//!
//! Every domain type shared across crates lives here.
//! We use **newtype wrappers** (`Symbol`, `Price`, `Quantity`) to
//! prevent mixing up values with the same underlying type (e.g.,
//! passing a quantity where a price is expected compiles cleanly
//! but is a logic bug — newtypes catch it at compile time).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

// ─────────────────────────────────────────────────────────────────
// Primitives
// ─────────────────────────────────────────────────────────────────

/// A trading-pair symbol, always stored upper-case (e.g. "BTCUSDT").
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Symbol(String);

impl Symbol {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into().to_uppercase())
    }
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn to_lowercase(&self) -> String { self.0.to_lowercase() }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for Symbol {
    fn from(s: &str) -> Self { Self::new(s) }
}

/// A price value — always non-negative, Decimal precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Price(pub Decimal);

impl Price {
    pub fn new(d: Decimal) -> Self { Self(d) }
    pub fn inner(self) -> Decimal { self.0 }
    pub fn to_f64(self) -> f64 { rust_decimal::prelude::ToPrimitive::to_f64(&self.0).unwrap_or(0.0) }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.6}", self.0)
    }
}

/// An asset quantity — always non-negative, Decimal precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Quantity(pub Decimal);

impl Quantity {
    pub fn new(d: Decimal) -> Self { Self(d) }
    pub fn inner(self) -> Decimal { self.0 }
}

// ─────────────────────────────────────────────────────────────────
// Candle  (OHLCV + extra Binance fields)
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candle {
    pub symbol:           Symbol,
    pub open_time:        DateTime<Utc>,
    pub open:             Price,
    pub high:             Price,
    pub low:              Price,
    pub close:            Price,
    pub volume:           Quantity,
    pub quote_volume:     Decimal,
    pub taker_buy_volume: Decimal,
    pub is_closed:        bool,
}

impl Candle {
    /// Percentage body size relative to candle range (0–100).
    pub fn body_pct(&self) -> f64 {
        let range = (self.high.0 - self.low.0).abs();
        if range.is_zero() { return 0.0; }
        let body = (self.close.0 - self.open.0).abs();
        rust_decimal::prelude::ToPrimitive::to_f64(&(body / range * Decimal::from(100))).unwrap_or(0.0)
    }
    pub fn is_bullish(&self) -> bool { self.close.0 > self.open.0 }
    pub fn is_bearish(&self) -> bool { self.close.0 < self.open.0 }
}

// ─────────────────────────────────────────────────────────────────
// Kline interval
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interval {
    OneMinute,
    FiveMinutes,
    FifteenMinutes,
    OneHour,
    FourHours,
    OneDay,
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::OneMinute      => "1m",
            Self::FiveMinutes    => "5m",
            Self::FifteenMinutes => "15m",
            Self::OneHour        => "1h",
            Self::FourHours      => "4h",
            Self::OneDay         => "1d",
        };
        write!(f, "{s}")
    }
}

// ─────────────────────────────────────────────────────────────────
// Trade signal
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction { Long, Short }

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Direction::Long  => write!(f, "LONG"),
            Direction::Short => write!(f, "SHORT"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetupKind {
    OrderBlockBounce,
    FairValueGapFill,
    BreakOfStructure,
    ChangeOfCharacter,
    LiquiditySweepReversal,
    RsiDivergence,
    VolumeBreakout,
}

impl fmt::Display for SetupKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::OrderBlockBounce      => "Order Block Bounce",
            Self::FairValueGapFill      => "FVG Fill",
            Self::BreakOfStructure      => "Break of Structure",
            Self::ChangeOfCharacter     => "Change of Character",
            Self::LiquiditySweepReversal => "Liquidity Sweep",
            Self::RsiDivergence         => "RSI Divergence",
            Self::VolumeBreakout        => "Volume Breakout",
        };
        write!(f, "{s}")
    }
}

/// A fully-formed trade signal ready for execution or paper-recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub id:          Uuid,
    pub symbol:      Symbol,
    pub direction:   Direction,
    pub kind:        SetupKind,
    pub entry:       Price,
    pub take_profit: Price,
    pub stop_loss:   Price,
    pub risk_reward: f64,
    pub confidence:  f64,    // 0.0 – 1.0
    pub size_usd:    Decimal, // Kelly-computed position size
    pub reasons:     Vec<String>,
    pub created_at:  DateTime<Utc>,
}

impl Signal {
    pub fn risk_pct(&self) -> f64 {
        if self.entry.0.is_zero() { return 0.0; }
        let r = (self.entry.0 - self.stop_loss.0).abs() / self.entry.0 * Decimal::from(100);
        rust_decimal::prelude::ToPrimitive::to_f64(&r).unwrap_or(0.0)
    }
    pub fn reward_pct(&self) -> f64 {
        if self.entry.0.is_zero() { return 0.0; }
        let r = (self.take_profit.0 - self.entry.0).abs() / self.entry.0 * Decimal::from(100);
        rust_decimal::prelude::ToPrimitive::to_f64(&r).unwrap_or(0.0)
    }
}

// ─────────────────────────────────────────────────────────────────
// Trade outcome
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome { Open, Win, Loss }

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Open => write!(f, "OPEN"),
            Outcome::Win  => write!(f, "WIN"),
            Outcome::Loss => write!(f, "LOSS"),
        }
    }
}

/// A closed (or still-open) paper/live trade record.
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub signal:      Signal,
    pub outcome:     Outcome,
    pub exit_price:  Option<Price>,
    pub pnl_usd:     Option<f64>,
    pub closed_at:   Option<DateTime<Utc>>,
}

// ─────────────────────────────────────────────────────────────────
// Performance metrics
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub total:          usize,
    pub wins:           usize,
    pub losses:         usize,
    pub win_rate:       f64,
    pub avg_win_usd:    f64,
    pub avg_loss_usd:   f64,
    pub profit_factor:  f64,
    pub total_pnl:      f64,
    pub max_drawdown:   f64,
    pub kelly_fraction: f64,
    pub capital:        f64,
}

impl Metrics {
    pub fn grade(&self) -> &'static str {
        match self.win_rate {
            r if r >= 0.62 => "A+",
            r if r >= 0.57 => "A",
            r if r >= 0.54 => "B",
            r if r >= 0.50 => "C",
            _              => "D",
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Coin scan result
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CoinScore {
    pub symbol:        Symbol,
    pub composite:     f64,
    pub volume_24h:    f64,
    pub change_24h:    f64,
    pub volatility:    f64,
    pub smc_score:     f64,
    pub trend_bullish: bool,
}

// ─────────────────────────────────────────────────────────────────
// Order request / result
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum OrderSide { Buy, Sell }

#[derive(Debug, Clone)]
pub struct OrderRequest {
    pub symbol:     Symbol,
    pub side:       OrderSide,
    pub quote_qty:  Decimal, // amount in quote currency (USDT)
}

#[derive(Debug, Clone)]
pub struct OrderResult {
    pub order_id: String,
    pub symbol:   Symbol,
    pub side:     OrderSide,
    pub qty:      Decimal,
    pub price:    Price,
    pub status:   String,
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_normalises_to_uppercase() {
        let s = Symbol::new("btcusdt");
        assert_eq!(s.as_str(), "BTCUSDT");
    }

    #[test]
    fn symbol_display() {
        assert_eq!(Symbol::new("ethusdt").to_string(), "ETHUSDT");
    }

    #[test]
    fn candle_bullish_bearish() {
        let make = |open: f64, close: f64| Candle {
            symbol:       Symbol::new("BTCUSDT"),
            open_time:    Utc::now(),
            open:         Price::new(Decimal::try_from(open).unwrap()),
            high:         Price::new(Decimal::try_from(close.max(open) + 10.0).unwrap()),
            low:          Price::new(Decimal::try_from(close.min(open) - 10.0).unwrap()),
            close:        Price::new(Decimal::try_from(close).unwrap()),
            volume:       Quantity::new(Decimal::ONE),
            quote_volume: Decimal::ONE,
            taker_buy_volume: Decimal::ONE,
            is_closed:    true,
        };
        assert!(make(100.0, 110.0).is_bullish());
        assert!(make(110.0, 100.0).is_bearish());
    }

    #[test]
    fn metrics_grade() {
        let mut m = Metrics::default();
        m.win_rate = 0.65; assert_eq!(m.grade(), "A+");
        m.win_rate = 0.56; assert_eq!(m.grade(), "A");
        m.win_rate = 0.52; assert_eq!(m.grade(), "B");
        m.win_rate = 0.48; assert_eq!(m.grade(), "D");
    }
}
