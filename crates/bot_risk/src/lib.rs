//! # bot_risk
//!
//! All pre-trade safety checks and position-sizing logic.
//!
//! The `RiskManager` is the **last gate** before any order is sent.
//! If `pre_trade_check` returns `Err`, the signal is discarded.

use bot_config::Config;
use bot_db::Store;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{info, warn};

// ── Error ─────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum RiskError {
    #[error("Circuit breaker active — trading halted until reset")]
    CircuitBreaker,
    #[error("Daily loss cap hit (${loss:.2} loss vs ${cap:.2} cap)")]
    DailyLossCap { loss: f64, cap: f64 },
    #[error("Max drawdown exceeded ({dd:.1}% vs {max:.1}% limit)")]
    MaxDrawdown { dd: f64, max: f64 },
}

// ── Manager ───────────────────────────────────────────────────────

pub struct RiskManager {
    config:  Arc<Config>,
    db:      Arc<dyn Store>,
    breaker: RwLock<bool>,
}

impl RiskManager {
    pub fn new(config: Arc<Config>, db: Arc<dyn Store>) -> Self {
        Self { config, db, breaker: RwLock::new(false) }
    }

    // ── Pre-trade gate ────────────────────────────────────────────

    /// Returns `Ok(())` if trading is permitted, `Err(RiskError)`
    /// if any safety constraint is violated.
    pub async fn pre_trade_check(&self) -> Result<(), RiskError> {
        // 1. Circuit breaker (manual or auto-tripped)
        if *self.breaker.read().await {
            return Err(RiskError::CircuitBreaker);
        }

        let capital    = self.db.capital().await;
        let daily_pnl  = self.db.daily_pnl().await;
        let metrics    = self.db.metrics().await;

        // 2. Daily loss cap
        let cap = capital * self.config.max_daily_loss_pct;
        if daily_pnl < -cap {
            self.trip_breaker("daily loss cap").await;
            return Err(RiskError::DailyLossCap { loss: -daily_pnl, cap });
        }

        // 3. Maximum drawdown
        if metrics.max_drawdown > self.config.max_drawdown_pct {
            self.trip_breaker("max drawdown exceeded").await;
            return Err(RiskError::MaxDrawdown {
                dd:  metrics.max_drawdown * 100.0,
                max: self.config.max_drawdown_pct * 100.0,
            });
        }

        Ok(())
    }

    // ── Position sizing ───────────────────────────────────────────

    /// Returns the USD amount to risk on the next trade, computed
    /// using the **quarter-Kelly criterion**:
    ///
    /// ```text
    /// f* = (p·b − q) / b        (full Kelly)
    /// size = capital × (f* × 0.25)
    /// ```
    ///
    /// Bounded to `[1.0, capital × max_trade_size_pct]`.
    pub async fn position_size(&self) -> f64 {
        let m = self.db.metrics().await;
        let size = m.capital * m.kelly_fraction;
        let min  = 1.0_f64;
        let max  = m.capital * self.config.max_trade_size_pct;
        size.clamp(min, max)
    }

    // ── Manual controls ───────────────────────────────────────────

    /// Trip the circuit breaker with a logged reason.
    pub async fn trip_breaker(&self, reason: &str) {
        warn!(reason, "Circuit breaker tripped");
        *self.breaker.write().await = true;
    }

    /// Clear the circuit breaker (call after manual review or cool-down).
    pub async fn reset_breaker(&self) {
        info!("Circuit breaker reset");
        *self.breaker.write().await = false;
    }

    pub async fn is_halted(&self) -> bool {
        *self.breaker.read().await
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Kelly formula sanity check (pure math, no async needed)
    #[test]
    fn kelly_formula_positive_edge() {
        // 55% win rate, 2:1 avg win/loss
        let p = 0.55_f64;
        let b = 2.0_f64;
        let f = (p * b - (1.0 - p)) / b;
        // f* ≈ 0.275  →  quarter-Kelly ≈ 0.069 (6.9%)
        let quarter = f * 0.25;
        assert!(quarter > 0.0 && quarter < 0.10,
            "quarter-Kelly={quarter:.3}");
    }

    #[test]
    fn kelly_negative_edge_is_zero_or_negative() {
        let p = 0.40_f64;
        let b = 1.0_f64;
        let f = (p * b - (1.0 - p)) / b;
        assert!(f < 0.0, "f={f}");
    }
}
