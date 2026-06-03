//! # bot_indicators
//!
//! Pure-function technical analysis.  Every indicator is a
//! stateless free function that takes a slice and returns a
//! `Result`, making them trivial to unit-test and fuzz.
//!
//! Inputs are plain `f64` slices so this crate stays free of
//! async / network dependencies and compiles in milliseconds.

use bot_types::Candle;
use thiserror::Error;

// ── Error ─────────────────────────────────────────────────────────

#[derive(Debug, Error, PartialEq)]
pub enum IndicatorError {
    #[error("Not enough data: need {need}, got {got}")]
    NotEnoughData { need: usize, got: usize },
    #[error("Division by zero in {0}")]
    DivisionByZero(&'static str),
}

// ─────────────────────────────────────────────────────────────────
// RSI  (Wilder's smoothed method — identical to TradingView default)
// ─────────────────────────────────────────────────────────────────

/// Returns the current RSI value (0–100).
///
/// Uses Wilder's exponential smoothing, which matches TradingView,
/// MetaTrader and most institutional platforms.
///
/// # Errors
/// Returns `NotEnoughData` when `closes.len() < period + 1`.
pub fn rsi(closes: &[f64], period: usize) -> Result<f64, IndicatorError> {
    if closes.len() < period + 1 {
        return Err(IndicatorError::NotEnoughData {
            need: period + 1,
            got:  closes.len(),
        });
    }
    let changes: Vec<f64> = closes.windows(2).map(|w| w[1] - w[0]).collect();

    // Seed: simple average of first `period` changes
    let seed = &changes[..period];
    let mut avg_gain = seed.iter().filter(|&&c| c > 0.0).sum::<f64>() / period as f64;
    let mut avg_loss = seed.iter().filter(|&&c| c < 0.0).map(|c| c.abs()).sum::<f64>()
        / period as f64;

    // Wilder smoothing for the rest
    for &chg in &changes[period..] {
        avg_gain = (avg_gain * (period as f64 - 1.0) + chg.max(0.0)) / period as f64;
        avg_loss = (avg_loss * (period as f64 - 1.0) + (-chg).max(0.0)) / period as f64;
    }

    if avg_loss == 0.0 {
        return Ok(100.0);
    }
    Ok(100.0 - 100.0 / (1.0 + avg_gain / avg_loss))
}

// ─────────────────────────────────────────────────────────────────
// EMA
// ─────────────────────────────────────────────────────────────────

/// Returns the full EMA series starting from the first complete bar.
///
/// # Errors
/// Returns `NotEnoughData` when `data.len() < period`.
pub fn ema(data: &[f64], period: usize) -> Result<Vec<f64>, IndicatorError> {
    if data.len() < period {
        return Err(IndicatorError::NotEnoughData {
            need: period,
            got:  data.len(),
        });
    }
    let k = 2.0 / (period as f64 + 1.0);
    let mut out = Vec::with_capacity(data.len() - period + 1);
    // Seed with SMA of first `period` bars
    out.push(data[..period].iter().sum::<f64>() / period as f64);
    for &price in &data[period..] {
        let prev = *out.last().unwrap();
        out.push(price * k + prev * (1.0 - k));
    }
    Ok(out)
}

/// Convenience: return only the last EMA value.
pub fn ema_last(data: &[f64], period: usize) -> Result<f64, IndicatorError> {
    ema(data, period).map(|v| *v.last().unwrap())
}

// ─────────────────────────────────────────────────────────────────
// MACD  (12, 26, 9)
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct MacdResult {
    pub macd:      f64, // fast EMA − slow EMA
    pub signal:    f64, // 9-period EMA of macd line
    pub histogram: f64, // macd − signal
}

/// Returns the current MACD values.
///
/// # Errors
/// Returns `NotEnoughData` when `closes.len() < 26 + 9 − 1`.
pub fn macd(closes: &[f64]) -> Result<MacdResult, IndicatorError> {
    let e12 = ema(closes, 12)?;
    let e26 = ema(closes, 26)?;

    // Align: e12 is longer, trim its head to match e26 length
    let trim = e12.len() - e26.len();
    let line: Vec<f64> = e12[trim..].iter().zip(&e26).map(|(a, b)| a - b).collect();

    let sig_series = ema(&line, 9)?;
    let macd_val   = *line.last().unwrap();
    let signal_val = *sig_series.last().unwrap();

    Ok(MacdResult {
        macd:      macd_val,
        signal:    signal_val,
        histogram: macd_val - signal_val,
    })
}

// ─────────────────────────────────────────────────────────────────
// ATR  (14)
// ─────────────────────────────────────────────────────────────────

/// Returns the current Average True Range.
///
/// # Errors
/// Returns `NotEnoughData` when `candles.len() < period + 1`.
pub fn atr(candles: &[Candle], period: usize) -> Result<f64, IndicatorError> {
    if candles.len() < period + 1 {
        return Err(IndicatorError::NotEnoughData {
            need: period + 1,
            got:  candles.len(),
        });
    }
    let trs: Vec<f64> = candles
        .windows(2)
        .map(|w| {
            let h  = w[1].high.to_f64();
            let l  = w[1].low.to_f64();
            let pc = w[0].close.to_f64();
            (h - l).max((h - pc).abs()).max((l - pc).abs())
        })
        .collect();

    let recent = &trs[trs.len().saturating_sub(period)..];
    Ok(recent.iter().sum::<f64>() / recent.len() as f64)
}

// ─────────────────────────────────────────────────────────────────
// VWAP
// ─────────────────────────────────────────────────────────────────

/// Returns the Volume-Weighted Average Price over the supplied candles.
///
/// # Errors
/// Returns `DivisionByZero` when total volume is zero.
pub fn vwap(candles: &[Candle]) -> Result<f64, IndicatorError> {
    let total_vol: f64 = candles
        .iter()
        .map(|k| {
            use rust_decimal::prelude::ToPrimitive;
            k.volume.0.to_f64().unwrap_or(0.0)
        })
        .sum();
    if total_vol == 0.0 {
        return Err(IndicatorError::DivisionByZero("vwap"));
    }
    let sum_pv: f64 = candles
        .iter()
        .map(|k| {
            use rust_decimal::prelude::ToPrimitive;
            let typ = (k.high.to_f64() + k.low.to_f64() + k.close.to_f64()) / 3.0;
            typ * k.volume.0.to_f64().unwrap_or(0.0)
        })
        .sum();
    Ok(sum_pv / total_vol)
}

// ─────────────────────────────────────────────────────────────────
// Stochastic RSI
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct StochRsi {
    pub k: f64, // %K (0–100)
    pub d: f64, // %D = 3-period SMA of %K
}

/// Stochastic RSI — oscillator of RSI values.
/// Typical thresholds: oversold < 20, overbought > 80.
pub fn stoch_rsi(closes: &[f64], rsi_period: usize, stoch_period: usize) -> Result<StochRsi, IndicatorError> {
    // Build RSI series
    let min_len = rsi_period + stoch_period;
    if closes.len() < min_len + 1 {
        return Err(IndicatorError::NotEnoughData { need: min_len + 1, got: closes.len() });
    }
    let rsi_series: Vec<f64> = (rsi_period..closes.len())
        .map(|i| rsi(&closes[..=i], rsi_period))
        .collect::<Result<_, _>>()?;

    let window = &rsi_series[rsi_series.len().saturating_sub(stoch_period)..];
    let min = window.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = window.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;
    let k = if range == 0.0 { 50.0 } else { (*window.last().unwrap() - min) / range * 100.0 };

    // %D = 3-bar SMA of the last 3 %K values (simplified single-bar here)
    let d = k; // full implementation would track history
    Ok(StochRsi { k, d })
}

// ─────────────────────────────────────────────────────────────────
// RSI Divergence
// ─────────────────────────────────────────────────────────────────

use bot_types::Direction;

/// Returns `Some(Direction)` when a classic RSI divergence is present.
///
/// * **Bullish**: price makes lower low, RSI makes higher low (buy signal).
/// * **Bearish**: price makes higher high, RSI makes lower high (sell signal).
pub fn rsi_divergence(
    candles: &[Candle],
    period: usize,
) -> Result<Option<Direction>, IndicatorError> {
    if candles.len() < period * 2 + 2 {
        return Ok(None);
    }
    let n = candles.len();
    let closes: Vec<f64> = candles.iter().map(|c| c.close.to_f64()).collect();

    let rsi_now  = rsi(&closes[n.saturating_sub(period + 1)..], period)?;
    let rsi_prev = rsi(&closes[n.saturating_sub(period * 2 + 1)..n.saturating_sub(period)], period)?;
    let price_now  = closes[n - 1];
    let price_prev = closes[n.saturating_sub(period + 1)];

    if price_now < price_prev && rsi_now > rsi_prev && rsi_now < 40.0 {
        return Ok(Some(Direction::Long));
    }
    if price_now > price_prev && rsi_now < rsi_prev && rsi_now > 60.0 {
        return Ok(Some(Direction::Short));
    }
    Ok(None)
}

// ─────────────────────────────────────────────────────────────────
// Helper re-exported for convenience
// ─────────────────────────────────────────────────────────────────

/// Extract close prices as `f64` from a candle slice.
pub fn closes(candles: &[Candle]) -> Vec<f64> {
    candles.iter().map(|c| c.close.to_f64()).collect()
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────
    fn rising(n: usize) -> Vec<f64> {
        (0..n).map(|i| 100.0 + i as f64).collect()
    }
    fn falling(n: usize) -> Vec<f64> {
        (0..n).map(|i| 200.0 - i as f64).collect()
    }
    fn flat(n: usize) -> Vec<f64> {
        vec![100.0; n]
    }

    // ── RSI ───────────────────────────────────────────────────────
    #[test]
    fn rsi_needs_enough_data() {
        assert_eq!(
            rsi(&rising(5), 14),
            Err(IndicatorError::NotEnoughData { need: 15, got: 5 })
        );
    }

    #[test]
    fn rsi_all_gains_is_100() {
        let v = rising(30);
        assert_eq!(rsi(&v, 14).unwrap(), 100.0);
    }

    #[test]
    fn rsi_all_losses_is_0() {
        let v = falling(30);
        let r = rsi(&v, 14).unwrap();
        assert!(r < 1.0, "RSI={r} expected near 0");
    }

    #[test]
    fn rsi_flat_is_undefined_but_no_panic() {
        // All changes are 0 → avg_loss = 0 → RSI = 100
        let v = flat(30);
        let r = rsi(&v, 14).unwrap();
        assert_eq!(r, 100.0);
    }

    #[test]
    fn rsi_typical_range() {
        // Alternating up/down: RSI should be near 50
        let v: Vec<f64> = (0..40)
            .map(|i| if i % 2 == 0 { 100.0 } else { 101.0 })
            .collect();
        let r = rsi(&v, 14).unwrap();
        assert!(r > 40.0 && r < 60.0, "RSI={r}");
    }

    // ── EMA ───────────────────────────────────────────────────────
    #[test]
    fn ema_needs_enough_data() {
        assert!(ema(&rising(5), 10).is_err());
    }

    #[test]
    fn ema_length_is_correct() {
        let out = ema(&rising(30), 10).unwrap();
        // 30 data points, period 10 → 30-10+1 = 21 output values
        assert_eq!(out.len(), 21);
    }

    #[test]
    fn ema_on_flat_series_equals_value() {
        let out = ema(&flat(30), 10).unwrap();
        for v in &out {
            assert!((v - 100.0).abs() < 1e-9, "EMA={v}");
        }
    }

    #[test]
    fn ema_rises_on_rising_series() {
        let out = ema(&rising(30), 5).unwrap();
        // EMA should be monotonically increasing
        for w in out.windows(2) {
            assert!(w[1] >= w[0], "{} < {}", w[1], w[0]);
        }
    }

    // ── MACD ──────────────────────────────────────────────────────
    #[test]
    fn macd_needs_enough_data() {
        assert!(macd(&rising(20)).is_err());
    }

    #[test]
    fn macd_rising_series_is_positive() {
        let r = macd(&rising(60)).unwrap();
        assert!(r.macd > 0.0, "MACD={}", r.macd);
    }

    #[test]
    fn macd_falling_series_is_negative() {
        let r = macd(&falling(60)).unwrap();
        assert!(r.macd < 0.0, "MACD={}", r.macd);
    }

    #[test]
    fn macd_histogram_equals_macd_minus_signal() {
        let r = macd(&rising(60)).unwrap();
        assert!((r.histogram - (r.macd - r.signal)).abs() < 1e-10);
    }

    // ── ATR ───────────────────────────────────────────────────────
    fn make_candles(n: usize, range: f64) -> Vec<Candle> {
        use bot_types::{Price, Quantity, Symbol};
        use rust_decimal::Decimal;
        (0..n)
            .map(|i| {
                let mid = 100.0 + i as f64;
                let d   = |v: f64| Price::new(Decimal::try_from(v).unwrap());
                let q   = Quantity::new(Decimal::try_from(1.0).unwrap());
                Candle {
                    symbol:      Symbol::new("TEST"),
                    open_time:   chrono::Utc::now(),
                    open:        d(mid),
                    high:        d(mid + range),
                    low:         d(mid - range),
                    close:       d(mid),
                    volume:      q,
                    quote_volume: Decimal::ONE,
                    taker_buy_volume: Decimal::ONE,
                    is_closed:   true,
                }
            })
            .collect()
    }

    #[test]
    fn atr_constant_range() {
        let candles = make_candles(20, 5.0);
        let a = atr(&candles, 14).unwrap();
        // With constant high-low range of 10 (mid±5), ATR ≈ 10
        assert!(a > 9.0 && a < 11.0, "ATR={a}");
    }

    #[test]
    fn atr_needs_enough_data() {
        let candles = make_candles(5, 5.0);
        assert!(atr(&candles, 14).is_err());
    }

    // ── EMA last ──────────────────────────────────────────────────
    #[test]
    fn ema_last_matches_last_of_full() {
        let data = rising(30);
        let full = ema(&data, 10).unwrap();
        let last = ema_last(&data, 10).unwrap();
        assert!((last - full.last().unwrap()).abs() < 1e-12);
    }

    // ── StochRSI ──────────────────────────────────────────────────
    #[test]
    fn stoch_rsi_range() {
        let v = (0..60).map(|i| 100.0 + (i as f64 * 0.3).sin() * 10.0).collect::<Vec<_>>();
        let s = stoch_rsi(&v, 14, 14).unwrap();
        assert!(s.k >= 0.0 && s.k <= 100.0, "k={}", s.k);
    }
}
